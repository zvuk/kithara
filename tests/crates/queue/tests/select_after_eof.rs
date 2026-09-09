#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZero;

use kithara::{
    self,
    events::TrackStatus,
    platform::sync::Arc,
    play::Resource,
    queue::{QueueControl, Transition, test_utils::QueueProbe},
    signal::AudioSpec,
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    offline::{
        OfflinePlayerHarness, mean_abs, offline_queue_fixture, resource_from_reader_with_src,
    },
};
use kithara_test_fixtures::integration_fixtures::{constant_loud, constant_quiet, constant_three};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
/// ≈ 3 s of rendered audio — plenty for a 0.4 s track.
const BLOCK_BUDGET: usize = 256;

#[derive(Clone, Copy)]
enum InitialStart {
    Play,
    Select,
}

fn make_resource(label: &str, secs: f64, samples: &'static [u8]) -> Resource {
    let spec = AudioSpec {
        channels: CHANNELS,
        sample_rate: NonZero::new(SAMPLE_RATE).unwrap(),
    };
    resource_from_reader_with_src(
        TestPcmReader::from_pcm(spec, secs, samples),
        Arc::from(format!("memory://{label}")),
    )
}

fn first_onset_frame(pcm: &[f32], threshold: f32) -> Option<usize> {
    let channels = usize::from(CHANNELS);
    pcm.chunks_exact(channels)
        .position(|frame| frame.iter().any(|s| s.abs() > threshold))
}

async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayerHarness,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::new();
    for _ in 0..block_budget {
        let _ = harness.run(queue, |q| q.tick()).await;
        let block = harness.render(BLOCK_FRAMES).await;
        pcm.extend(block);
    }
    pcm
}

#[kithara::test(tokio)]
async fn seek_updates_cached_position_optimistically(constant_quiet: &'static [u8]) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let id = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("seek", 120.0, constant_quiet))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select track");

    queue.seek(54.689_879_542).expect("seek must land");

    assert_eq!(queue.position_seconds(), Some(54.689_879_542));
    drop(queue);
    harness.close().await;
}

/// Track A plays to natural EOF while B is stuck loading, so auto-advance
/// only stashes a pending select. Re-selecting A must restart it: the old
/// `rate() > 0` guard kept reporting "playing" after EOF and swallowed it.
#[kithara::test(tokio)]
async fn reselect_finished_track_restarts_when_next_track_never_loads(
    constant_three: &'static [u8],
) {
    const TRACK_SECS: f64 = 0.4;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_three))
        })
        .await;
    // Registered but never completed: stands in for a stalled loader.
    let _id_b = queue.register_for_test();

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    let first_pcm = render_loop(&queue, &harness, BLOCK_BUDGET).await;
    assert!(
        first_onset_frame(&first_pcm, 0.005).is_some(),
        "track A must play through on the first pass"
    );

    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, constant_three));
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select of the finished track must be accepted");

    let second_pcm = render_loop(&queue, &harness, BLOCK_BUDGET).await;
    assert!(
        first_onset_frame(&second_pcm, 0.005).is_some(),
        "re-selecting track A after it played to EOF must restart playback \
         — the select was silently swallowed by the already-playing guard"
    );
    assert_eq!(
        queue.current_index(),
        Some(0),
        "queue must stay on track A after the restart"
    );
    drop(queue);
    harness.close().await;
}

/// Switching back to a `Consumed` track while another track is audibly
/// playing must switch the *audio*, not just the bookkeeping: A (quiet)
/// must dominate the output after the switch-back, B (loud) must stop.
#[kithara::test(tokio)]
#[case::selected(InitialStart::Select)]
#[case::play_button(InitialStart::Play)]
async fn switch_back_to_consumed_track_switches_audio(
    #[case] initial_start: InitialStart,
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    const TRACK_SECS: f64 = 8.0;
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_quiet))
        })
        .await;
    let id_b = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("b", TRACK_SECS, constant_loud))
        })
        .await;

    match initial_start {
        InitialStart::Play => harness.run(&queue, move |q| q.play()).await,
        InitialStart::Select => harness
            .run(&queue, move |q| q.select(id_a, Transition::None))
            .await
            .expect("select track A"),
    }
    let pcm_a = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    let mean_a = mean_abs(&pcm_a[pcm_a.len() / 2..]);
    assert!(mean_a > 0.005, "track A must start: mean={mean_a}");

    harness
        .run(&queue, move |q| q.select(id_b, Transition::None))
        .await
        .expect("select track B");
    let pcm_b = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    let mean_b = mean_abs(&pcm_b[pcm_b.len() / 2..]);
    assert!(
        mean_b > mean_a * 4.0,
        "track B must dominate after the switch: mean_a={mean_a}, mean_b={mean_b}"
    );

    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, constant_quiet));
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("switch back to track A");

    // Skip the first half of the window: switch latency and the cf=0 cut.
    let pcm = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    let mean_back = mean_abs(&pcm[pcm.len() / 2..]);
    assert!(
        mean_back > 0.005,
        "track A must be audible after the switch-back: mean={mean_back}"
    );
    assert!(
        mean_back < mean_b / 4.0,
        "track B must stop sounding after the switch-back — the UI switched \
         but the audio kept playing B: mean_b={mean_b}, mean_back={mean_back}"
    );
    assert_eq!(queue.current_index(), Some(0));
    drop(queue);
    harness.close().await;
}

/// `play()` hands the current item's resource to the engine, so the
/// queue must mirror that: the current `Loaded` track becomes `Consumed`,
/// keeping the status truthful for later re-selects.
#[kithara::test(tokio)]
async fn play_button_marks_current_loaded_track_consumed(constant_three: &'static [u8]) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", 8.0, constant_three))
        })
        .await;
    let _id_b = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("b", 8.0, constant_three))
        })
        .await;

    harness.run(&queue, move |q| q.play()).await;
    let _ = render_loop(&queue, &harness, 8).await;

    let status_a = queue
        .tracks()
        .iter()
        .find(|e| e.id == id_a)
        .map(|e| e.status.clone())
        .expect("track A in queue");
    assert_eq!(
        status_a,
        TrackStatus::Consumed,
        "play() consumed track A's slot resource; its status must say so"
    );
    drop(queue);
    harness.close().await;
}

/// Re-selecting the playing track must cancel a pending switch so a
/// stalled load finishing later cannot barge in on top of it.
#[kithara::test(tokio)]
async fn reselect_playing_track_cancels_pending_switch(constant_three: &'static [u8]) {
    const TRACK_SECS: f64 = 5.0;
    /// Enough blocks to confirm audible playback without reaching EOF.
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_three))
        })
        .await;
    let id_b = queue.register_for_test();

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    let warmup_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    assert!(
        first_onset_frame(&warmup_pcm, 0.005).is_some(),
        "track A must be audibly playing before the pending-switch step"
    );

    harness
        .run(&queue, move |q| q.select(id_b, Transition::None))
        .await
        .expect("select of a still-loading track stashes a pending switch");
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select of the playing track");

    let status_b = queue
        .tracks()
        .iter()
        .find(|e| e.id == id_b)
        .map(|e| e.status.clone())
        .expect("track B still in the queue");
    assert_eq!(
        status_b,
        TrackStatus::Cancelled,
        "re-selecting the playing track must cancel the pending switch so \
         track B's late-finishing load cannot barge in"
    );

    let after_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    assert!(
        first_onset_frame(&after_pcm, 0.005).is_some(),
        "track A must keep playing uninterrupted"
    );
    drop(queue);
    harness.close().await;
}
