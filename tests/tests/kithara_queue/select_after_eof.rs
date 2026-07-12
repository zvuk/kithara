#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZero;

use kithara::{
    self,
    decode::PcmSpec,
    events::TrackStatus,
    platform::sync::Arc,
    play::{PlayerConfig, Resource},
    queue::{Queue, QueueConfig, Transition},
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader, offline::resource_from_reader_with_src,
};

use super::offline_player_harness::OfflinePlayerHarness;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
/// ≈ 3 s of rendered audio — plenty for a 0.4 s track.
const BLOCK_BUDGET: usize = 256;

fn make_resource(label: &str, secs: f64, value: f32) -> Resource {
    let spec = PcmSpec {
        channels: CHANNELS,
        sample_rate: NonZero::new(SAMPLE_RATE).unwrap(),
    };
    resource_from_reader_with_src(
        TestPcmReader::with_value(spec, secs, value),
        Arc::from(format!("memory://{label}")),
    )
}

fn mean_abs(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    pcm.iter().map(|s| s.abs()).sum::<f32>() / pcm.len() as f32
}

fn first_onset_frame(pcm: &[f32], threshold: f32) -> Option<usize> {
    let channels = usize::from(CHANNELS);
    pcm.chunks_exact(channels)
        .position(|frame| frame.iter().any(|s| s.abs() > threshold))
}

fn render_loop(queue: &Queue, harness: &OfflinePlayerHarness, block_budget: usize) -> Vec<f32> {
    let mut pcm = Vec::new();
    for _ in 0..block_budget {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        pcm.extend(block);
    }
    pcm
}

fn make_fixture() -> (OfflinePlayerHarness, Queue) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let mut config = QueueConfig::default().with_player(Arc::clone(harness.player()));
    config.should_autoplay = false;
    let queue = Queue::new(config);
    (harness, queue)
}

#[kithara::test]
fn seek_updates_cached_position_optimistically() {
    let (_harness, queue) = make_fixture();
    let id = queue.insert_loaded_for_test(make_resource("seek", 120.0, 0.10));
    queue.select(id, Transition::None).expect("select track");

    queue.seek(54.689_879_542).expect("seek must land");

    assert_eq!(queue.position_seconds(), Some(54.689_879_542));
}

/// Track A plays to natural EOF while B is stuck loading, so auto-advance
/// only stashes a pending select. Re-selecting A must restart it: the old
/// `rate() > 0` guard kept reporting "playing" after EOF and swallowed it.
#[kithara::test(tokio)]
async fn reselect_finished_track_restarts_when_next_track_never_loads() {
    const TRACK_SECS: f64 = 0.4;
    const TRACK_VALUE: f32 = 0.30;

    let (harness, queue) = make_fixture();

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, TRACK_VALUE));
    // Registered but never completed: stands in for a stalled loader.
    let _id_b = queue.register_for_test();

    queue
        .select(id_a, Transition::None)
        .expect("select track A");
    let first_pcm = render_loop(&queue, &harness, BLOCK_BUDGET);
    assert!(
        first_onset_frame(&first_pcm, 0.005).is_some(),
        "track A must play through on the first pass"
    );

    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, TRACK_VALUE));
    queue
        .select(id_a, Transition::None)
        .expect("re-select of the finished track must be accepted");

    let second_pcm = render_loop(&queue, &harness, BLOCK_BUDGET);
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
}

/// Switching back to a `Consumed` track while another track is audibly
/// playing must switch the *audio*, not just the bookkeeping: A (quiet)
/// must dominate the output after the switch-back, B (loud) must stop.
#[kithara::test(tokio)]
async fn switch_back_to_consumed_track_switches_audio() {
    const TRACK_SECS: f64 = 8.0;
    const QUIET: f32 = 0.10;
    const LOUD: f32 = 0.80;
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = make_fixture();

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, QUIET));
    let id_b = queue.insert_loaded_for_test(make_resource("b", TRACK_SECS, LOUD));

    queue
        .select(id_a, Transition::None)
        .expect("select track A");
    let pcm_a = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let mean_a = mean_abs(&pcm_a[pcm_a.len() / 2..]);
    assert!(mean_a > 0.005, "track A must be audible: mean={mean_a}");

    queue
        .select(id_b, Transition::None)
        .expect("select track B");
    let pcm_b = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let mean_b = mean_abs(&pcm_b[pcm_b.len() / 2..]);
    assert!(
        mean_b > mean_a * 4.0,
        "track B must dominate after the switch: mean_a={mean_a}, mean_b={mean_b}"
    );

    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, QUIET));
    queue
        .select(id_a, Transition::None)
        .expect("switch back to track A");

    // Skip the first half of the window: switch latency and the cf=0 cut.
    let pcm = render_loop(&queue, &harness, WARMUP_BLOCKS);
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
}

/// The user's exact steps: press Play (not select!), double-click track 2,
/// double-click track 1. `play()` consumes the slot resource behind the
/// queue's back, so track 1 still reads `Loaded`; the switch-back must
/// nevertheless switch the *audio* back to track 1.
#[kithara::test(tokio)]
async fn play_button_then_switch_away_and_back_switches_audio() {
    const TRACK_SECS: f64 = 8.0;
    const QUIET: f32 = 0.10;
    const LOUD: f32 = 0.80;
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = make_fixture();

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, QUIET));
    let id_b = queue.insert_loaded_for_test(make_resource("b", TRACK_SECS, LOUD));

    // Step 1: the GUI Play button — playback starts without a select.
    queue.play();
    let pcm_a = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let mean_a = mean_abs(&pcm_a[pcm_a.len() / 2..]);
    assert!(mean_a > 0.005, "Play must start track A: mean={mean_a}");

    // Step 2: double-click track 2.
    queue
        .select(id_b, Transition::None)
        .expect("select track B");
    let pcm_b = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let mean_b = mean_abs(&pcm_b[pcm_b.len() / 2..]);
    assert!(
        mean_b > mean_a * 4.0,
        "track B must dominate: mean_a={mean_a}, mean_b={mean_b}"
    );

    // Step 3: double-click track 1.
    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, QUIET));
    queue
        .select(id_a, Transition::None)
        .expect("switch back to track A");

    let pcm = render_loop(&queue, &harness, WARMUP_BLOCKS);
    let mean_back = mean_abs(&pcm[pcm.len() / 2..]);
    assert!(
        mean_back > 0.005,
        "track A must sound again after the switch-back: mean={mean_back}"
    );
    assert!(
        mean_back < mean_b / 4.0,
        "track B must stop after the switch-back — Play consumed A's slot \
         behind the queue's back and the re-select silently loaded nothing: \
         mean_b={mean_b}, mean_back={mean_back}"
    );
    assert_eq!(queue.current_index(), Some(0));
}

/// `play()` hands the current item's resource to the engine, so the
/// queue must mirror that: the current `Loaded` track becomes `Consumed`,
/// keeping the status truthful for later re-selects.
#[kithara::test(tokio)]
async fn play_button_marks_current_loaded_track_consumed() {
    let (harness, queue) = make_fixture();
    let id_a = queue.insert_loaded_for_test(make_resource("a", 8.0, 0.3));
    let _id_b = queue.insert_loaded_for_test(make_resource("b", 8.0, 0.3));

    queue.play();
    let _ = render_loop(&queue, &harness, 8);

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
}

/// Re-selecting the playing track must cancel a pending switch so a
/// stalled load finishing later cannot barge in on top of it.
#[kithara::test(tokio)]
async fn reselect_playing_track_cancels_pending_switch() {
    const TRACK_SECS: f64 = 5.0;
    const TRACK_VALUE: f32 = 0.30;
    /// Enough blocks to confirm audible playback without reaching EOF.
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = make_fixture();

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, TRACK_VALUE));
    let id_b = queue.register_for_test();

    queue
        .select(id_a, Transition::None)
        .expect("select track A");
    let warmup_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS);
    assert!(
        first_onset_frame(&warmup_pcm, 0.005).is_some(),
        "track A must be audibly playing before the pending-switch step"
    );

    queue
        .select(id_b, Transition::None)
        .expect("select of a still-loading track stashes a pending switch");
    queue
        .select(id_a, Transition::None)
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

    let after_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS);
    assert!(
        first_onset_frame(&after_pcm, 0.005).is_some(),
        "track A must keep playing uninterrupted"
    );
}
