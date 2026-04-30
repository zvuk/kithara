//! End-to-end auto-advance tests: real `Queue` + offline render + PCM check.
//!
//! Each test inserts two distinguishable PCM tracks (different constant
//! signal value), drives the system through `Queue::tick()` (the same
//! call kithara-app and the FFI use), captures the rendered output, and
//! asserts the second track's signature is present after the first one
//! ends. Catches regressions where the queue notices the protocol-level
//!  events, but the audio thread never actually promotes the next track.
//!
//! Bypasses the network loader via `Queue::insert_loaded_for_test`; the
//! real `Resource::new` URL fetch path is covered by `real_playlist.rs`.
//!
//! Cases:
//! - `cf_zero_queue_tick_advances_to_second_track_audio` — cf=0 path
//!   (arena handover at EOF).
//! - `cf_nonzero_queue_tick_crossfades_to_second_track_audio` — cf>0
//!   path (queue commits on `HandoverRequested`).

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use kithara_audio::mock::TestPcmReader;
use kithara_decode::PcmSpec;
use kithara_play::{
    PlayerConfig, Resource,
    internal::offline::resource_from_reader_with_src,
};
use kithara_queue::{Queue, QueueConfig, Transition};
use kithara_test_utils::kithara;

use super::offline_player_harness::OfflinePlayerHarness;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const MAX_BLOCKS: usize = 1024;

fn make_resource(label: &str, secs: f64, value: f32) -> Resource {
    let spec = PcmSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
    };
    resource_from_reader_with_src(
        TestPcmReader::with_value(spec, secs, value),
        Arc::from(format!("memory://{label}")),
    )
}

/// Average absolute amplitude over a window of `frames` frames starting at
/// `frame_offset`. Returns `None` if the window does not fit.
fn mean_abs_window(pcm: &[f32], frame_offset: usize, frames: usize) -> Option<f32> {
    let channels = usize::from(CHANNELS);
    let start = frame_offset.checked_mul(channels)?;
    let end = start.checked_add(frames.checked_mul(channels)?)?;
    if end > pcm.len() {
        return None;
    }
    let window = &pcm[start..end];
    let sum: f32 = window.iter().map(|s| s.abs()).sum();
    Some(sum / window.len() as f32)
}

/// First frame where `|sample|` rises above `threshold`. The audio
/// thread takes a few blocks to start producing samples after `select`,
/// so windows must be measured relative to this onset, not frame 0.
fn first_onset_frame(pcm: &[f32], threshold: f32) -> Option<usize> {
    let channels = usize::from(CHANNELS);
    pcm.chunks_exact(channels)
        .position(|frame| frame.iter().any(|s| s.abs() > threshold))
}


/// Render until either EOF count or block budget is reached. Returns the
/// concatenated stereo-interleaved PCM.
fn render_loop(
    queue: &Queue,
    harness: &OfflinePlayerHarness,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::new();
    for _ in 0..block_budget {
        let _ = queue.tick();
        let block = harness.render(BLOCK_FRAMES);
        pcm.extend(block);
    }
    pcm
}

/// cf=0: queue.tick must drive `process_notifications`, the audio thread
/// arena handover at EOF promotes the armed next track, and the second
/// track's PCM signal must replace the first one's.
#[kithara::test(tokio)]
async fn cf_zero_queue_tick_advances_to_second_track_audio() {
    const TRACK_SECS: f64 = 0.4;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(0.0),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(false),
    );

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, TRACK_A_VALUE));
    let _ = queue.insert_loaded_for_test(make_resource("b", TRACK_SECS, TRACK_B_VALUE));
    queue.select(id_a, Transition::None).expect("select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS);

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence within the render budget");
    let track_a_frames = (f64::from(SAMPLE_RATE) * TRACK_SECS) as usize;
    let window = SAMPLE_RATE as usize / 8;

    // Window inside track A (skip first 1/4 to clear fade-in).
    let mean_a = mean_abs_window(&pcm, onset + track_a_frames / 4, window)
        .expect("track A mid window fits");

    // Window inside where track B should be (well past A's end + handover).
    let track_b_probe = onset + track_a_frames + track_a_frames / 4;
    let mean_b = mean_abs_window(&pcm, track_b_probe, window)
        .expect("track B mid window fits — render budget too small");

    // The firewheel graph applies a fixed master attenuation
    // (~`Volume::default()³`) so we compare ratios, not absolute values:
    // mean_b / mean_a must mirror TRACK_B_VALUE / TRACK_A_VALUE within tol.
    let expected_ratio = TRACK_B_VALUE / TRACK_A_VALUE;
    let observed_ratio = mean_b / mean_a.max(f32::EPSILON);
    assert!(
        observed_ratio > expected_ratio * 0.7,
        "track B is not playing where it should — auto-advance likely broken. \
         expected ratio ≈ {expected_ratio}, got {observed_ratio} \
         (mean_a={mean_a}, mean_b={mean_b}, onset={onset}, probe_frame={track_b_probe})"
    );
    assert!(
        mean_a > 0.005,
        "track A produced no audible signal: mean_a={mean_a}"
    );
    assert!(
        mean_b > mean_a * 4.0,
        "track B amplitude must dominate track A's after auto-advance \
         (mean_a={mean_a}, mean_b={mean_b})"
    );

    assert_eq!(
        queue.current_index(),
        Some(1),
        "queue.current_index must follow the audio thread to track B"
    );
}

/// cf>0: queue.tick observes `HandoverRequested`, calls `commit_next`,
/// the two tracks overlap in the crossfade window and PCM mid-track-B
/// must show track B's value.
#[kithara::test(tokio)]
async fn cf_nonzero_queue_tick_crossfades_to_second_track_audio() {
    const TRACK_SECS: f64 = 1.5;
    const CROSSFADE_SECS: f32 = 0.3;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECS),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(false),
    );

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, TRACK_A_VALUE));
    let _ = queue.insert_loaded_for_test(make_resource("b", TRACK_SECS, TRACK_B_VALUE));
    queue.select(id_a, Transition::None).expect("select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS);

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence within the render budget");
    let track_a_frames = (f64::from(SAMPLE_RATE) * TRACK_SECS) as usize;
    let crossfade_frames = (f32::from(SAMPLE_RATE as u16) * CROSSFADE_SECS) as usize;
    let window = SAMPLE_RATE as usize / 8;

    // Window well inside track A (skip first 1/4 fade-in, well before the
    // trailing crossfade).
    let mean_a = mean_abs_window(&pcm, onset + track_a_frames / 4, window)
        .expect("track A early window fits");

    // Window inside track B, after the crossfade has settled.
    let track_b_probe = onset + track_a_frames + crossfade_frames * 2;
    let mean_b = mean_abs_window(&pcm, track_b_probe, window)
        .expect("track B settled window fits");

    let expected_ratio = TRACK_B_VALUE / TRACK_A_VALUE;
    let observed_ratio = mean_b / mean_a.max(f32::EPSILON);
    assert!(
        observed_ratio > expected_ratio * 0.7,
        "track B is not playing where it should — crossfade auto-advance likely broken. \
         expected ratio ≈ {expected_ratio}, got {observed_ratio} \
         (mean_a={mean_a}, mean_b={mean_b}, onset={onset}, probe_frame={track_b_probe})"
    );
    assert!(
        mean_a > 0.005,
        "track A produced no audible signal: mean_a={mean_a}"
    );
    assert!(
        mean_b > mean_a * 4.0,
        "track B amplitude must dominate track A's after crossfade commit \
         (mean_a={mean_a}, mean_b={mean_b})"
    );

    assert_eq!(
        queue.current_index(),
        Some(1),
        "queue.current_index must advance to track B after crossfade commit"
    );
}

/// Sanity guard: if `Queue::tick` regresses to skipping
/// `process_notifications`, this test must fail. We confirm the
/// fix-under-test by asserting both PrefetchRequested and HandoverRequested
/// reach the bus during a cf>0 cycle — purely event-level, but pinned to
/// the real `Queue::tick` path.
#[kithara::test(tokio)]
async fn queue_tick_pumps_audio_thread_notifications_to_bus() {
    use kithara_events::{Event, PlayerEvent};
    use kithara_platform::tokio::sync::broadcast::error::TryRecvError;

    const TRACK_SECS: f64 = 1.0;
    const CROSSFADE_SECS: f32 = 0.2;

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(CROSSFADE_SECS),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(false),
    );
    let mut rx = queue.subscribe();

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, 0.10));
    let _ = queue.insert_loaded_for_test(make_resource("b", TRACK_SECS, 0.80));
    queue.select(id_a, Transition::None).expect("select track A");

    let mut prefetch_seen = false;
    let mut handover_seen = false;
    let mut item_end_seen = false;

    for _ in 0..MAX_BLOCKS {
        let _ = queue.tick();
        let _ = harness.render(BLOCK_FRAMES);

        loop {
            match rx.try_recv() {
                Ok(Event::Player(PlayerEvent::PrefetchRequested)) => prefetch_seen = true,
                Ok(Event::Player(PlayerEvent::HandoverRequested)) => handover_seen = true,
                Ok(Event::Player(PlayerEvent::ItemDidPlayToEnd { .. })) => item_end_seen = true,
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if prefetch_seen && handover_seen && item_end_seen {
            break;
        }
    }

    assert!(
        prefetch_seen,
        "PrefetchRequested must reach the bus via Queue::tick → process_notifications"
    );
    assert!(
        handover_seen,
        "HandoverRequested must reach the bus via Queue::tick → process_notifications"
    );
    assert!(
        item_end_seen,
        "ItemDidPlayToEnd must reach the bus via Queue::tick → process_notifications"
    );
}

/// Behavioural autoplay test that **simulates the actual race** that
/// the production loader can produce: register two tracks in order
/// (A, B) with autoplay enabled, then force their load-completions in
/// the *opposite* order (B first, A second). With the original
/// race-prone implementation, B would win autoplay and play first; with
/// the synchronous-arm fix, A still plays first because the arm
/// happened at register-time, not at load-completion-time.
///
/// Track A is quiet, track B is loud — if B preempted A the early
/// window's mean amplitude would jump to B's level.
#[kithara::test(tokio)]
async fn autoplay_first_registered_track_plays_first_even_when_loaded_last() {
    const TRACK_SECS: f64 = 0.4;
    const QUIET_VALUE: f32 = 0.10; // track A — registered first, must play first
    const LOUD_VALUE: f32 = 0.80; // track B — registered second, loaded first

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(0.0),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(true),
    );

    // Register both BEFORE any load completes, exactly like the real
    // app calling `queue.append(url1); queue.append(url2);`.
    let id_a = queue.register_for_test();
    let id_b = queue.register_for_test();

    // Load completions arrive in REVERSE order — simulates B's loader
    // task winning the timing race against A's. The original
    // implementation would have picked B as the autoplay target.
    queue.complete_load_for_test(id_b, make_resource("b", TRACK_SECS, LOUD_VALUE));
    queue.complete_load_for_test(id_a, make_resource("a", TRACK_SECS, QUIET_VALUE));

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS);

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("autoplay must start producing audio without an explicit select");
    let track_a_frames = (f64::from(SAMPLE_RATE) * TRACK_SECS) as usize;
    let window = SAMPLE_RATE as usize / 8;

    let mean_first = mean_abs_window(&pcm, onset + track_a_frames / 4, window)
        .expect("window inside the first audible track fits");
    let track_second_probe = onset + track_a_frames + track_a_frames / 4;
    let mean_second = mean_abs_window(&pcm, track_second_probe, window)
        .expect("window inside the second audible track fits");

    // Regression check: if B preempted A, mean_first ≈ LOUD and
    // mean_second ≈ QUIET (or silence). With the fix, mean_first ≈
    // QUIET and mean_second ≈ LOUD. Compare via ratios so the firewheel
    // graph attenuation cancels out.
    assert!(
        mean_second > mean_first * 4.0,
        "the loud track (B, registered SECOND) preempted the quiet track (A, \
         registered FIRST) — autoplay race regression. \
         mean_first={mean_first}, mean_second={mean_second} \
         (expected first to be quiet ≈ {QUIET_VALUE}, second to be loud ≈ {LOUD_VALUE})"
    );

    assert_eq!(
        queue.current_index(),
        Some(1),
        "after track A finishes, queue must auto-advance to track B"
    );
}

/// Replay regression: after a full cf=0 playthrough every track is
/// `Consumed`. A second pass over the same queue must still
/// auto-advance — i.e. `handle_prefetch_requested` must respawn the
/// `Consumed` next-track via the loader path so `arm_next` can fire
/// again. Before the fix, the queue stopped after the first track on
/// every replay.
///
/// Drives the full production code path: the second `select` of track A
/// hits the `Consumed` branch in `Queue::select` (which respawns via
/// `spawn_apply_after_load`), then mid-A the prefetch trigger fires
/// for `Consumed` track B which my fix re-spawns. We pre-supply fresh
/// `Resource`s to the loader so spawn completes synthetically, mirroring
/// what a real network loader would deliver on a replay.
#[kithara::test(tokio)]
async fn cf_zero_replay_after_full_playthrough_still_advances() {
    const TRACK_SECS: f64 = 0.4;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(0.0),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(false),
    );

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, TRACK_A_VALUE));
    let id_b = queue.insert_loaded_for_test(make_resource("b", TRACK_SECS, TRACK_B_VALUE));

    // First playthrough: select A and let it auto-advance to B, then
    // end_queue when B finishes.
    queue.select(id_a, Transition::None).expect("first select track A");
    let _first_pcm = render_loop(&queue, &harness, MAX_BLOCKS);
    assert_eq!(
        queue.current_index(),
        Some(1),
        "first playthrough must reach track B"
    );

    // Pre-supply fresh resources for the replay. spawn_apply_after_load
    // pulls these instead of dispatching the real loader.
    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, TRACK_A_VALUE));
    queue.supply_test_resource_for_respawn(id_b, make_resource("b2", TRACK_SECS, TRACK_B_VALUE));

    // Replay: select A again. Goes through the `Consumed` branch in
    // `Queue::select` → respawns A → A becomes Loaded → pending_select
    // matches → plays. Mid-A, prefetch handler sees B=Consumed and my
    // fix respawns B → B becomes Loaded → arm_next → arena handover at
    // A's EOF promotes B.
    queue.select(id_a, Transition::None).expect("second select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS);

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence on replay");
    let track_a_frames = (f64::from(SAMPLE_RATE) * TRACK_SECS) as usize;
    let window = SAMPLE_RATE as usize / 8;

    let mean_a = mean_abs_window(&pcm, onset + track_a_frames / 4, window)
        .expect("track A mid window fits");
    let track_b_probe = onset + track_a_frames + track_a_frames / 4;
    let mean_b = mean_abs_window(&pcm, track_b_probe, window)
        .expect("track B mid window fits — the queue may have stopped after track A on replay");

    assert!(
        mean_b > mean_a * 4.0,
        "track B must play after track A on REPLAY (Consumed-respawn regression). \
         mean_a={mean_a}, mean_b={mean_b}"
    );

    assert_eq!(
        queue.current_index(),
        Some(1),
        "second playthrough must also reach track B"
    );
}

/// When the LAST track finishes via auto-advance, the queue must pause
/// the player so the UI sees a stopped state — otherwise `rate` stays
/// at the previous default and `is_playing()` keeps returning true on
/// a ghost position past the last EOF.
#[kithara::test(tokio)]
async fn queue_pauses_player_when_last_track_ends() {
    use kithara_events::{Event, QueueEvent};
    use kithara_platform::tokio::sync::broadcast::error::TryRecvError;

    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(0.0),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(false),
    );
    let mut rx = queue.subscribe();

    let id_a = queue.insert_loaded_for_test(make_resource("a", TRACK_SECS, 0.30));
    queue.select(id_a, Transition::None).expect("select track A");

    // Render until QueueEnded fires (or budget exhausts).
    let mut saw_queue_ended = false;
    for _ in 0..MAX_BLOCKS {
        let _ = queue.tick();
        let _ = harness.render(BLOCK_FRAMES);
        loop {
            match rx.try_recv() {
                Ok(Event::Queue(QueueEvent::QueueEnded)) => saw_queue_ended = true,
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if saw_queue_ended {
            // Drain a few more ticks to let pause propagate to the player.
            for _ in 0..4 {
                let _ = queue.tick();
                let _ = harness.render(BLOCK_FRAMES);
            }
            break;
        }
    }

    assert!(
        saw_queue_ended,
        "QueueEnded must fire when the last track finishes"
    );
    assert!(
        !queue.is_playing(),
        "player must be paused after the last track ends so UI shows a \
         stopped state — otherwise rate stays at default and `is_playing` \
         keeps returning true past the final EOF"
    );
}

/// Regression: `PrefetchRequested` can arrive before `current_index` is
/// written on autoplay start. If `peek_next` defaults `None` to `Some(0)`,
/// the prefetch handler arms slot 0 against the already-playing decoder.
#[kithara::test(tokio)]
async fn autoplay_first_track_does_not_self_arm_and_kill_its_own_decoder() {
    const TRACK_SECS: f64 = 0.4;
    const TRACK_VALUE: f32 = 0.30;

    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_crossfade_duration(0.0),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_autoplay(true),
    );

    // With the bug, peek_next(None) -> Some(0) arms this slot against itself.
    let _id = queue.insert_loaded_for_test(make_resource("solo", TRACK_SECS, TRACK_VALUE));

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS);

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("autoplay'd track must produce audible samples");
    let track_frames = (f64::from(SAMPLE_RATE) * TRACK_SECS) as usize;
    let window = SAMPLE_RATE as usize / 8;

    let mean_mid = mean_abs_window(&pcm, onset + track_frames / 4, window)
        .expect("mid window inside the track fits");

    assert!(
        mean_mid > 0.005,
        "no signal mid-playback (mean={mean_mid}) — decoder likely self-armed"
    );

    assert_eq!(queue.current_index(), Some(0), "current_index must stay on the only track");
}
