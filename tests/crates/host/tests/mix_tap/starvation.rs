use kithara::{
    abr::AbrMode,
    assets::AssetStore,
    download::{Downloader, DownloaderConfig},
    net::NetOptions,
    platform::{
        CancelToken,
        time::Duration,
        tokio::{
            runtime::Handle,
            sync::broadcast::{Receiver, error::TryRecvError},
        },
    },
    play::{ResourceConfig, ResourceSrc},
    queue::QueueControl,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    offline::{OfflinePlayer, OfflinePlayerOptions},
};
use kithara_render::bridge::{DeckEvent, SlotState};
use kithara_test_utils::cancel_token;

use super::cases::render_effect_window;

#[kithara::test(
    tokio,
    multi_thread,
    timeout(::std::time::Duration::from_millis(45140))
)]
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
    let mut raw = crate::underrun::Underruns::new(&harness);
    let mut edges = Vec::new();
    for (index, gate) in gates.iter().enumerate() {
        let expected = u64::try_from(index + 1).expect("three windows");
        let mut starved = false;
        for _ in 0..1024 {
            let _ = render_effect_window(&harness, 512).await;
            if harness.metrics().underruns() == expected {
                assert!(
                    gate.requested() > 0,
                    "the withheld input owns this starvation window"
                );
                let before_empty_callback = harness.host().position();
                let silence = render_effect_window(&harness, 512).await;
                assert_eq!(
                    harness.host().position(),
                    before_empty_callback + 512,
                    "empty callbacks advance the Host clock before input is released"
                );
                assert!(
                    silence.iter().all(|sample| *sample == 0.0),
                    "empty callbacks return silence"
                );
                let slot = harness
                    .deck_snapshot()
                    .slots
                    .into_iter()
                    .find(|slot| slot.binding.is_some_and(|binding| binding.item() == item))
                    .expect("the item keeps its native slot");
                assert_eq!(slot.state, SlotState::Playing);
                starved = true;
                break;
            }
        }
        assert!(starved, "the withheld segment must exhaust ready PCM");
        gate.release();
        let mut recovered = false;
        for _ in 0..256 {
            let output = render_effect_window(&harness, 512).await;
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
        drain_edges(&mut events, &mut edges);
        assert_eq!(edges.iter().filter(|event| matches!(event, DeckEvent::UnderrunStarted { item: owner, .. } if *owner == item)).count(), index + 1);
        assert_eq!(edges.iter().filter(|event| matches!(event, DeckEvent::UnderrunEnded { item: owner, recovered: true, .. } if *owner == item)).count(), index + 1);
    }
    raw.checkpoint(
        harness.metrics(),
        true,
        "three forced starvation windows recovered",
    );
    harness.with_queue(QueueControl::pause).await;
    let paused = render_effect_window(&harness, 2048).await;
    assert!(
        paused[paused.len() - 1024..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    raw.checkpoint(harness.metrics(), true, "after pause");
    harness.with_queue(QueueControl::play).await;
    let resumed = render_effect_window(&harness, 4096).await;
    assert!(resumed.iter().any(|sample| sample.abs() > 0.01));
    raw.checkpoint(harness.metrics(), true, "after play");
    assert_eq!(
        harness.metrics().underruns(),
        3,
        "only the three withheld segments starve before seek"
    );
    harness.seek(0.5).await;
    let sought = render_effect_window(&harness, 4096).await;
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
    drain_edges(&mut events, &mut edges);
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
