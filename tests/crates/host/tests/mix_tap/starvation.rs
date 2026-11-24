use kithara::{
    abr::AbrMode,
    assets::AssetStore,
    download::{Downloader, DownloaderConfig},
    events::TrackId,
    net::NetOptions,
    platform::{
        CancelToken,
        time::Duration,
        tokio::{
            runtime::Handle,
            sync::broadcast::{Receiver, error::TryRecvError},
        },
    },
    play::{DeckParameter, DeckProgram, ResourceConfig, ResourceSrc},
    queue::QueueControl,
};
use kithara_command::Seq;
use kithara_integration_tests::{
    HlsFixtureBuilder, SegmentGateHandle, TestServerHelper,
    offline::{OfflinePlayer, OfflinePlayerOptions},
    usdt_trace,
};
use kithara_render::bridge::{DeckEvent, SlotState};
use kithara_test_utils::cancel_token;

use super::cases::render_effect_window;
use crate::underrun::Underruns;

/// One deck program a false-mode starvation window issues while its segment
/// gate is still closed: the program to admit, the parameter it addresses,
/// and the value its last knot holds.
pub(crate) struct ProgramPlan {
    pub(crate) program: DeckProgram,
    pub(crate) parameter: DeckParameter,
    pub(crate) endpoint: f64,
}

/// Render an empty 512-frame window through the withheld deck. The caller
/// records program admission and the window in one live probe scope.
async fn empty_callback(harness: &OfflinePlayer) {
    let before = harness.host().position();
    let silence = render_effect_window(harness, 512).await;
    assert_eq!(
        harness.host().position(),
        before + 512,
        "empty callbacks advance the Host clock before input is released"
    );
    assert!(
        silence.iter().all(|sample| *sample == 0.0),
        "empty callbacks return silence"
    );
}

/// Observe an active program's published endpoint and owner while the segment
/// gate is still closed, returning the program owner and authority to re-check
/// unchanged through recovery. The authority is only set when a manual command
/// interrupts a program, so a pure program on an untouched field reports it as
/// `None`; the tuple is asserted unchanged, not `Some`.
fn assert_active(
    harness: &OfflinePlayer,
    parameter: DeckParameter,
    endpoint: f64,
) -> (Option<Seq>, Option<Seq>) {
    let field = harness
        .deck_snapshot()
        .parameter(parameter)
        .expect("the active program publishes its endpoint");
    assert!(
        (field
            .value()
            .expect("the program publishes its endpoint value")
            - endpoint)
            .abs()
            <= 1e-9,
        "the program holds its endpoint while the gate is still closed"
    );
    assert!(
        field.program().is_some(),
        "the active program publishes its owner"
    );
    (field.program(), field.authority())
}

/// Drive the three withheld-segment starvation windows on `harness`, issuing a
/// deck program on a window when `on_window` returns one for it, then verify
/// recovery, pause/play and seek on the same item.
pub(crate) async fn run_withheld_windows<F>(
    harness: &OfflinePlayer,
    item: TrackId,
    gates: &[SegmentGateHandle],
    events: &mut Receiver<DeckEvent>,
    raw: &mut Underruns,
    mut on_window: F,
) where
    F: FnMut(&OfflinePlayer, usize) -> Option<ProgramPlan>,
{
    let mut edges = Vec::new();
    let mut active: Option<ActiveProgram> = None;
    for (index, gate) in gates.iter().enumerate() {
        let expected = u64::try_from(index + 1).expect("three windows");
        starve_window(
            harness,
            item,
            gate,
            expected,
            index,
            &mut on_window,
            &mut active,
        )
        .await;
        recover_window(harness, item, gate, expected, active).await;
        drain_edges(events, &mut edges);
        assert_eq!(
            edges
                .iter()
                .filter(|event| matches!(event, DeckEvent::UnderrunStarted { item: owner, .. } if *owner == item))
                .count(),
            index + 1
        );
        assert_eq!(
            edges
                .iter()
                .filter(|event| matches!(event, DeckEvent::UnderrunEnded { item: owner, recovered: true, .. } if *owner == item))
                .count(),
            index + 1
        );
    }
    verify_transport(harness, item, events, &mut edges, raw).await;
}

/// The endpoint and owner of the program a window issued, carried across the
/// three windows so recovery re-checks them unchanged.
#[derive(Clone, Copy)]
struct ActiveProgram {
    endpoint: (DeckParameter, f64),
    owner: (Option<Seq>, Option<Seq>),
}

/// Exhaust ready PCM on one withheld segment, issuing the window's program when
/// `on_window` returns one, and assert the empty callback returns with the gate
/// still closed and a complete zero-wait trace.
async fn starve_window<F>(
    harness: &OfflinePlayer,
    item: TrackId,
    gate: &SegmentGateHandle,
    expected: u64,
    index: usize,
    on_window: &mut F,
    active: &mut Option<ActiveProgram>,
) where
    F: FnMut(&OfflinePlayer, usize) -> Option<ProgramPlan>,
{
    for _ in 0..1024 {
        let _ = render_effect_window(harness, 512).await;
        if harness.metrics().underruns() == expected {
            assert!(
                gate.requested() > 0,
                "the withheld input owns this starvation window"
            );
            let trace = usdt_trace::scope();
            if let Some(plan) = on_window(harness, index) {
                let ProgramPlan {
                    program,
                    parameter,
                    endpoint,
                } = plan;
                harness
                    .with_queue(move |queue| queue.deck_programs(vec![program]))
                    .await
                    .expect(
                        "the deck-program callback returns while the segment gate is still closed",
                    );
                empty_callback(harness).await;
                let owner = assert_active(harness, parameter, endpoint);
                *active = Some(ActiveProgram {
                    endpoint: (parameter, endpoint),
                    owner,
                });
            } else {
                empty_callback(harness).await;
            }
            let pcm_wait_started = trace.events_of("pcm_wait_started");
            let pcm_wait_finished = trace.events_of("pcm_wait_finished");
            let truncated = usdt_trace::recorded().1;
            drop(trace);
            assert!(
                pcm_wait_started.is_empty(),
                "the empty callback returns without waiting for PCM to start"
            );
            assert!(
                pcm_wait_finished.is_empty(),
                "the empty callback returns without waiting for PCM to finish"
            );
            assert!(!truncated, "the zero-wait trace is complete, not truncated");
            let slot = harness
                .deck_snapshot()
                .slots
                .into_iter()
                .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
                .expect("the item keeps its native slot");
            assert_eq!(slot.state, SlotState::Playing);
            return;
        }
    }
    panic!("the withheld segment must exhaust ready PCM");
}

/// Release the segment gate and assert recovery: audible output resumes, the
/// same item is still current, exactly one underrun happened, and any program
/// endpoint and owner survived.
async fn recover_window(
    harness: &OfflinePlayer,
    item: TrackId,
    gate: &SegmentGateHandle,
    expected: u64,
    active: Option<ActiveProgram>,
) {
    gate.release();
    let mut recovered = false;
    for _ in 0..256 {
        let output = render_effect_window(harness, 512).await;
        if output.iter().any(|sample| sample.abs() > 0.01) {
            recovered = true;
            break;
        }
    }
    assert!(recovered, "PCM resumes without another Play or load");
    assert_eq!(harness.player().current().expect("current item").id, item);
    assert_eq!(
        harness.metrics().underruns(),
        expected,
        "one window per withheld segment"
    );
    if let Some(ActiveProgram {
        endpoint: (parameter, endpoint),
        owner,
    }) = active
    {
        let field = harness
            .deck_snapshot()
            .parameter(parameter)
            .expect("the endpoint survives recovery");
        assert!(
            (field.value().expect("the endpoint value survives recovery") - endpoint).abs() <= 1e-9,
            "the program endpoint is unchanged through recovery"
        );
        assert_eq!(
            (field.program(), field.authority()),
            owner,
            "the program owner is unchanged through recovery"
        );
    }
}

/// Verify pause/play and seek keep the same item bound and audible, and that no
/// decode error, event overflow, or eviction tainted the run.
async fn verify_transport(
    harness: &OfflinePlayer,
    item: TrackId,
    events: &mut Receiver<DeckEvent>,
    edges: &mut Vec<DeckEvent>,
    raw: &mut Underruns,
) {
    raw.checkpoint(
        harness.metrics(),
        true,
        "three forced starvation windows recovered",
    );
    harness.with_queue(QueueControl::pause).await;
    let paused = render_effect_window(harness, 2048).await;
    assert!(
        paused[paused.len() - 1024..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    raw.checkpoint(harness.metrics(), true, "after pause");
    harness.with_queue(QueueControl::play).await;
    let resumed = render_effect_window(harness, 4096).await;
    assert!(resumed.iter().any(|sample| sample.abs() > 0.01));
    raw.checkpoint(harness.metrics(), true, "after play");
    assert_eq!(
        harness.metrics().underruns(),
        3,
        "only the three withheld segments starve before seek"
    );
    harness.seek(0.5).await;
    let sought = render_effect_window(harness, 4096).await;
    assert!(sought.iter().any(|sample| sample.abs() > 0.01));
    assert!(
        (0.49..0.75).contains(&harness.position()),
        "seek changed the same item's audible source position: {}",
        harness.position()
    );
    assert_eq!(
        harness.player().current().expect("same controlled item").id,
        item
    );
    assert_eq!(harness.metrics().decode_errors(), 0);
    assert_eq!(harness.metrics().event_overflows(), 0);
    assert_eq!(harness.metrics().evicted_playing(), 0);
    raw.checkpoint(harness.metrics(), true, "after seek");
    drain_edges(events, edges);
    assert!(
        edges.iter().all(|event| matches!(event,
            DeckEvent::UnderrunStarted { item: owner, .. }
            | DeckEvent::UnderrunEnded { item: owner, recovered: true, .. } if *owner == item
        )),
        "every observed window belongs to the same item and ends by recovery: {edges:?}"
    );
    let slot = harness
        .deck_snapshot()
        .slots
        .into_iter()
        .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
        .expect("the controlled item remains bound");
    assert_eq!(slot.state, SlotState::Playing);
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
async fn a_queue_item_recovers_from_three_withheld_segments_and_keeps_its_transport(
    cancel_token: CancelToken,
) {
    let server = TestServerHelper::new().await;
    let hls = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(8)
                .segment_duration_secs(2.0)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("packaged audio source");
    let gates = [1, 3, 5].map(|segment| server.register_segment_gate(hls.token(), 0, segment));
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(false)
            .build(),
        44_100,
    )
    .await;
    let pools = harness.worker().pools().clone();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(TestServerHelper::shared_client(
            NetOptions::default(),
            cancel_token.child(),
        ))
        .runtime(Handle::current())
        .build(),
    );
    let config = ResourceConfig::for_src(ResourceSrc::Url(hls.master_url()))
        .downloader(downloader)
        .store(AssetStore::builder(pools).build())
        .initial_abr_mode(AbrMode::manual(0))
        .build();
    let mut events = harness.subscribe_underruns();
    let item = harness.load_config(config).await;
    let mut raw = Underruns::new(&harness);
    run_withheld_windows(&harness, item, &gates, &mut events, &mut raw, |_, _| None).await;
    harness.close().await;
}

fn drain_edges(receiver: &mut Receiver<DeckEvent>, edges: &mut Vec<DeckEvent>) {
    loop {
        match receiver.try_recv() {
            Ok(event) => edges.push(event),
            Err(TryRecvError::Empty) => break,
            Err(error) => panic!("raw starvation events lost: {error}"),
        }
    }
}
