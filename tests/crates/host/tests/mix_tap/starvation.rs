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
    signal::SegmentId,
    stream::AudioCodec,
};
use kithara_command::Seq;
use kithara_integration_tests::{
    HlsFixtureBuilder, SegmentGateHandle, TestServerHelper,
    offline::{OfflinePlayer, OfflinePlayerOptions},
    test_defaults::packaged_content_frames,
    usdt_trace,
};
use kithara_render::bridge::{DeckEvent, DeckSnapshot, Slot, SlotState};
use kithara_test_fixtures::hls_fixtures::frame_samples;
use kithara_test_utils::{cancel_token, wait_until};
use num_traits::ToPrimitive;

use crate::underrun::Underruns;

const CALLBACK_FRAMES: usize = 64;

/// One deck program a false-mode starvation window issues while its segment
/// gate is still closed: the program to admit, the parameter it addresses,
/// and the value its last knot holds.
pub(crate) struct ProgramPlan {
    pub(crate) program: DeckProgram,
    pub(crate) parameter: DeckParameter,
    pub(crate) endpoint: f64,
}

async fn render_unpaced_window(harness: &OfflinePlayer, frames: usize) -> Vec<f32> {
    let mut output = Vec::with_capacity(frames * 2);
    for start in (0..frames).step_by(CALLBACK_FRAMES) {
        let block = CALLBACK_FRAMES.min(frames - start);
        output.extend_from_slice(&harness.render(block).await);
    }
    output
}

/// Render an empty 512-frame window through the withheld deck. The caller
/// records program admission and the window in one live probe scope.
async fn empty_callback(harness: &OfflinePlayer) {
    let before = harness.host().position();
    let silence = render_unpaced_window(harness, 512).await;
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
    let mut origin = None;
    let mut active: Option<ActiveProgram> = None;
    for (index, gate) in gates.iter().enumerate() {
        let expected = u64::try_from(index + 1).expect("three windows");
        starve_window(
            harness,
            item,
            gate,
            index,
            &mut on_window,
            &mut active,
            (events, &mut edges, &mut origin),
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

fn required_packaged_position(index: usize) -> Duration {
    let frame_samples = frame_samples(AudioCodec::AacLc).expect("AAC encoder frame geometry");
    let segment_frames = packaged_content_frames(2 * 44_100, frame_samples, 1)
        .expect("packet-aligned starvation segment");
    let segment = index * 2 + 1;
    let frames = (segment_frames * segment)
        .to_f64()
        .expect("packaged starvation position fits f64");
    Duration::from_secs_f64(frames / f64::from(44_100_u32))
}

async fn wait_for_callback_or_withheld_boundary(
    harness: &OfflinePlayer,
    item: TrackId,
    gate: &SegmentGateHandle,
    required: Duration,
) {
    wait_until(
        Duration::from_secs(10),
        "withheld segment producer readiness",
        || {
            if gate.requested() == 0 {
                return true;
            }
            let required = required.as_secs_f64();
            let callback =
                f64::from(u32::try_from(CALLBACK_FRAMES).expect("callback frames fit u32"))
                    / 44_100.0;
            harness
                .deck_snapshot()
                .slots
                .iter()
                .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
                .is_none_or(|slot| {
                    slot.position >= required
                        || slot.frontier >= required
                        || slot.frontier - slot.position >= callback
                })
        },
    )
    .await
    .unwrap_or_else(|error| panic!("withheld segment producer readiness: {error}"));
}

#[derive(Clone, Copy)]
struct OriginWitness {
    slot: Slot,
    item: TrackId,
    attachment: u64,
    epoch: u64,
    segment: SegmentId,
    source_revision: u64,
    origin: Duration,
    raw_position: Duration,
}

fn source_witness(snapshot: &DeckSnapshot, item: TrackId) -> Result<OriginWitness, String> {
    let mut matching = snapshot
        .slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.binding.is_some_and(|binding| binding.item() == item));
    let (index, slot) = matching
        .next()
        .ok_or_else(|| format!("item {item:?} has no slot witness"))?;
    if matching.next().is_some() {
        return Err(format!("item {item:?} has multiple slot witnesses"));
    }
    let binding = slot
        .binding
        .ok_or_else(|| format!("item {item:?} lost its slot binding"))?;
    let source = slot
        .source
        .ok_or_else(|| format!("item {item:?} has no source metadata"))?;
    let mark = slot
        .mark
        .ok_or_else(|| format!("item {item:?} has no raw source mark"))?;
    if source.segment != binding.segment() || mark.lane.segment != binding.segment() {
        return Err(format!("item {item:?} has mismatched source segments"));
    }
    if source.spec.sample_rate.get() != 44_100 {
        return Err(format!("item {item:?} changed its source sample rate"));
    }
    Ok(OriginWitness {
        slot: Slot::new(u16::try_from(index).map_err(|_| "slot index exceeds Slot".to_owned())?),
        item,
        attachment: binding.attachment().get(),
        epoch: binding.epoch().into(),
        segment: binding.segment(),
        source_revision: source.source_revision,
        origin: source
            .media_origin
            .ok_or_else(|| format!("item {item:?} has no actual media origin"))?,
        raw_position: mark.position,
    })
}

fn require_starvation_boundary(
    edges: &[DeckEvent],
    witness: OriginWitness,
    stable: &mut Option<OriginWitness>,
    item: TrackId,
    expected: u64,
    index: usize,
    required: Duration,
) -> Result<(), String> {
    let starts: Vec<(Slot, Option<Duration>)> = edges
        .iter()
        .filter_map(|event| match event {
            DeckEvent::UnderrunStarted {
                slot,
                item: owner,
                position,
                ..
            } if *owner == item => Some((*slot, *position)),
            _ => None,
        })
        .collect();
    if starts.len() != usize::try_from(expected).expect("window count fits usize") {
        return Err(format!(
            "expected {expected} underrun starts before window {index}, got {}",
            starts.len()
        ));
    }
    let (slot, position) = starts
        .last()
        .copied()
        .ok_or_else(|| format!("window {index} has no underrun start"))?;
    let position =
        position.ok_or_else(|| format!("window {index} has no owner source position"))?;
    if witness.slot != slot || witness.item != item {
        return Err(format!("window {index} has a mismatched item/slot witness"));
    }
    let raw = position
        .checked_add(witness.origin)
        .ok_or_else(|| format!("window {index} raw source position overflows"))?;
    if raw != witness.raw_position {
        return Err(format!(
            "window {index} event/source raw positions differ: event={raw:?}, slot={:?}",
            witness.raw_position
        ));
    }
    if raw < required {
        return Err(format!(
            "window {index} underrun precedes its withheld packaged boundary: raw={raw:?}, required={required:?}, origin={:?}",
            witness.origin
        ));
    }
    if let Some(previous) = *stable
        && (previous.slot != witness.slot
            || previous.item != witness.item
            || previous.attachment != witness.attachment
            || previous.epoch != witness.epoch
            || previous.segment != witness.segment
            || previous.source_revision != witness.source_revision
            || previous.origin != witness.origin)
    {
        return Err(format!("window {index} changed its source or media origin"));
    }
    *stable = Some(witness);
    Ok(())
}

/// Exhaust ready PCM on one withheld segment, issuing the window's program when
/// `on_window` returns one, and assert the empty callback returns with the gate
/// still closed and a complete zero-wait trace.
async fn starve_window<F>(
    harness: &OfflinePlayer,
    item: TrackId,
    gate: &SegmentGateHandle,
    index: usize,
    on_window: &mut F,
    active: &mut Option<ActiveProgram>,
    observation: (
        &mut Receiver<DeckEvent>,
        &mut Vec<DeckEvent>,
        &mut Option<OriginWitness>,
    ),
) where
    F: FnMut(&OfflinePlayer, usize) -> Option<ProgramPlan>,
{
    let expected = u64::try_from(index + 1).expect("three windows");
    let required = required_packaged_position(index);
    let (events, edges, origin) = observation;
    for _ in 0..1024 {
        for _ in 0..512 / CALLBACK_FRAMES {
            wait_for_callback_or_withheld_boundary(harness, item, gate, required).await;
            let _ = harness.render(CALLBACK_FRAMES).await;
        }
        if harness.metrics().underruns() == expected {
            assert!(
                gate.requested() > 0,
                "the withheld input owns this starvation window"
            );
            drain_edges(events, edges);
            source_witness(&harness.deck_snapshot(), item)
                .and_then(|witness| {
                    require_starvation_boundary(
                        edges, witness, origin, item, expected, index, required,
                    )
                })
                .unwrap_or_else(|error| panic!("gate-owned starvation precondition: {error}"));
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
        let output = render_unpaced_window(harness, 512).await;
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
    let paused = render_unpaced_window(harness, 2048).await;
    assert!(
        paused[paused.len() - 1024..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    raw.checkpoint(harness.metrics(), true, "after pause");
    harness.with_queue(QueueControl::play).await;
    let resumed = render_unpaced_window(harness, 4096).await;
    assert!(resumed.iter().any(|sample| sample.abs() > 0.01));
    raw.checkpoint(harness.metrics(), true, "after play");
    assert_eq!(
        harness.metrics().underruns(),
        3,
        "only the three withheld segments starve before seek"
    );
    harness.seek(0.5).await;
    let sought = render_unpaced_window(harness, 4096).await;
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

#[kithara::test]
fn starvation_rejects_pre_boundary_producer_lag() {
    use kithara::signal::SessionFrame;

    let item = TrackId(1);
    let event = |position| DeckEvent::UnderrunStarted {
        slot: Slot::new(0),
        item,
        at: SessionFrame::new(0),
        position: Some(position),
    };
    let required = Duration::from_secs_f64(89_088.0 / 44_100.0);
    let origin = Duration::from_millis(37);
    let witness = OriginWitness {
        slot: Slot::new(0),
        item,
        attachment: 1,
        epoch: 1,
        segment: SegmentId::FIRST,
        source_revision: 7,
        origin,
        raw_position: required,
    };
    let mut stable = None;
    assert!(source_witness(&DeckSnapshot::default(), item).is_err());

    let pre_boundary = required - Duration::from_millis(1);
    let mut edges = vec![event(pre_boundary - origin)];
    assert!(
        require_starvation_boundary(
            &edges,
            OriginWitness {
                raw_position: pre_boundary,
                ..witness
            },
            &mut stable,
            item,
            1,
            0,
            required,
        )
        .is_err()
    );

    edges[0] = event(required - origin);
    assert!(
        require_starvation_boundary(&edges, witness, &mut stable, item, 1, 0, required).is_ok()
    );

    let changed_origin = origin + Duration::from_millis(1);
    edges[0] = event(required - changed_origin);
    assert!(
        require_starvation_boundary(
            &edges,
            OriginWitness {
                origin: changed_origin,
                ..witness
            },
            &mut stable,
            item,
            1,
            0,
            required,
        )
        .is_err()
    );

    edges[0] = event(required - origin);
    assert!(
        require_starvation_boundary(
            &edges,
            OriginWitness {
                slot: Slot::new(1),
                ..witness
            },
            &mut stable,
            item,
            1,
            0,
            required,
        )
        .is_err()
    );

    edges.push(event(required - origin));
    assert!(
        require_starvation_boundary(&edges, witness, &mut stable, item, 1, 0, required).is_err()
    );
}

#[kithara::test(tokio, multi_thread, timeout(Long))]
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
