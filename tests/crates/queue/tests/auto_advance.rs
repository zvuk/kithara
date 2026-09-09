#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    self,
    events::{AdvanceReason, Event, QueueEvent},
    platform::sync::Arc,
    play::Resource,
    queue::{Queue, QueueConfig, QueueControl, RepeatMode, Transition, test_utils::QueueProbe},
    signal::AudioSpec,
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions, resource_from_reader_with_src},
};
use kithara_test_fixtures::integration_fixtures::{
    constant_four, constant_loud, constant_quiet, constant_three, constant_two,
};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const MAX_BLOCKS: usize = 1024;

fn with_autoplay(
    mut config: QueueConfig<TestPools>,
    should_autoplay: bool,
) -> QueueConfig<TestPools> {
    config.should_autoplay = should_autoplay;
    config
}

fn make_resource(label: &str, secs: f64, samples: &'static [u8]) -> Resource {
    let spec = AudioSpec::new(CHANNELS, NonZeroU32::new(SAMPLE_RATE).expect("test rate"));
    resource_from_reader_with_src(
        TestPcmReader::from_pcm(spec, secs, samples),
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
async fn crossfade_started_requires_a_live_predecessor(constant_three: &'static [u8]) {
    const CROSSFADE_SECS: f32 = 0.2;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;
    let id = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("initial", 0.2, constant_three))
        })
        .await;
    let mut receiver = queue.subscribe();

    harness
        .run(&queue, move |q| q.select(id, Transition::Crossfade))
        .await
        .expect("select initial track");

    while let Ok(envelope) = receiver.try_recv() {
        assert!(
            !matches!(
                envelope.event,
                Event::Queue(QueueEvent::CrossfadeStarted { .. })
            ),
            "a cold select cannot crossfade from an idle player"
        );
    }

    let mut saw_playing = false;
    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, |q| q.tick()).await;
        let _ = harness.render(BLOCK_FRAMES).await;
        let is_playing = queue.is_playing();
        saw_playing |= is_playing;
        if saw_playing && !is_playing {
            break;
        }
    }
    assert!(
        saw_playing,
        "the predecessor must start before reaching EOF"
    );
    assert!(!queue.is_playing(), "the predecessor must reach EOF");
    harness
        .run(&queue, |q| q.tick())
        .await
        .expect("process predecessor EOF");

    let successor = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("successor", 1.0, constant_three))
        })
        .await;
    let mut receiver = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(successor, Transition::Crossfade))
        .await
        .expect("select successor after EOF");

    while let Ok(envelope) = receiver.try_recv() {
        assert!(
            !matches!(
                envelope.event,
                Event::Queue(QueueEvent::CrossfadeStarted { .. })
            ),
            "a completed predecessor cannot start a crossfade"
        );
    }
    drop(queue);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn repeat_one_natural_advance_keeps_current_track(constant_three: &'static [u8]) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;
    let id = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("one", 1.0, constant_three))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select repeat-one track");
    let mut receiver = queue.subscribe();
    queue.set_repeat(RepeatMode::One);

    assert!(matches!(
        receiver.try_recv().map(|envelope| envelope.event),
        Ok(Event::Queue(QueueEvent::RepeatModeChanged {
            mode: kithara::events::QueueRepeatMode::One,
        }))
    ));
    assert_eq!(
        harness
            .run(&queue, move |q| q.advance_to_next(
                Transition::Crossfade,
                AdvanceReason::NaturalEof
            ))
            .await
            .expect("advance repeat-one queue"),
        Some(id)
    );
    assert_eq!(queue.current().map(|entry| entry.id), Some(id));
    drop(queue);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn repeat_all_natural_advance_wraps_last_track_to_first(
    constant_loud: &'static [u8],
    constant_two: &'static [u8],
) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;
    let first = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("first", 1.0, constant_two))
        })
        .await;
    let last = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("last", 1.0, constant_loud))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(last, Transition::None))
        .await
        .expect("select last repeat-all track");
    let mut receiver = queue.subscribe();
    queue.set_repeat(RepeatMode::All);

    assert!(matches!(
        receiver.try_recv().map(|envelope| envelope.event),
        Ok(Event::Queue(QueueEvent::RepeatModeChanged {
            mode: kithara::events::QueueRepeatMode::All,
        }))
    ));
    assert_eq!(
        harness
            .run(&queue, move |q| q.advance_to_next(
                Transition::Crossfade,
                AdvanceReason::NaturalEof
            ))
            .await
            .expect("advance repeat-all queue"),
        Some(first)
    );
    assert_eq!(queue.current().map(|entry| entry.id), Some(first));
    drop(queue);
    harness.close().await;
}

/// cf=0: queue.tick must drive `process_notifications`, the audio thread
/// arena handover at EOF promotes the armed next track, and the second
/// track's PCM signal must replace the first one's.
#[kithara::test(tokio)]
async fn cf_zero_queue_tick_advances_to_second_track_audio(
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    const TRACK_SECS: f64 = 0.4;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_quiet))
        })
        .await;
    let _ = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("b", TRACK_SECS, constant_loud))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence within the render budget");
    let track_a_frames =
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * TRACK_SECS).unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;

    let mean_a =
        mean_abs_window(&pcm, onset + track_a_frames / 4, window).expect("track A mid window fits");

    let track_b_probe = onset + track_a_frames + track_a_frames / 4;
    let mean_b = mean_abs_window(&pcm, track_b_probe, window)
        .expect("track B mid window fits — render budget too small");

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
    drop(queue);
    harness.close().await;
}

/// cf>0: queue.tick observes `HandoverRequested`, calls `commit_next`,
/// the two tracks overlap in the crossfade window and PCM mid-track-B
/// must show track B's value.
#[kithara::test(tokio)]
async fn cf_nonzero_queue_tick_crossfades_to_second_track_audio(
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    const TRACK_SECS: f64 = 1.5;
    const CROSSFADE_SECS: f32 = 0.3;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_quiet))
        })
        .await;
    let _ = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("b", TRACK_SECS, constant_loud))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence within the render budget");
    let track_a_frames =
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * TRACK_SECS).unwrap_or(usize::MAX);
    let crossfade_frames = num_traits::cast::<f32, usize>(
        f32::from(u16::try_from(SAMPLE_RATE).unwrap_or(u16::MAX)) * CROSSFADE_SECS,
    )
    .unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;

    let mean_a = mean_abs_window(&pcm, onset + track_a_frames / 4, window)
        .expect("track A early window fits");

    let track_b_probe = onset + track_a_frames + crossfade_frames * 2;
    let mean_b = mean_abs_window(&pcm, track_b_probe, window).expect("track B settled window fits");

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
    drop(queue);
    harness.close().await;
}

/// Sanity guard: if `Queue::tick` regresses to skipping
/// `process_notifications`, this test must fail. We confirm the
/// fix-under-test by asserting both `PrefetchRequested` and `HandoverRequested`
/// reach the bus during a cf>0 cycle — purely event-level, but pinned to
/// the real `Queue::tick` path.
#[kithara::test(tokio)]
async fn queue_tick_pumps_audio_thread_notifications_to_bus(
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    use kithara::{
        events::{Event, PlayerEvent},
        platform::tokio::sync::broadcast::error::TryRecvError,
    };

    const TRACK_SECS: f64 = 1.0;
    const CROSSFADE_SECS: f32 = 0.2;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;
    let mut rx = queue.subscribe();

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_quiet))
        })
        .await;
    let _ = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("b", TRACK_SECS, constant_loud))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let mut prefetch_seen = false;
    let mut handover_seen = false;
    let mut item_end_seen = false;

    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, |q| q.tick()).await;
        let _ = harness.render(BLOCK_FRAMES).await;

        loop {
            match rx.try_recv().map(|env| env.event) {
                Ok(Event::Player(PlayerEvent::PrefetchRequested)) => prefetch_seen = true,
                Ok(Event::Player(PlayerEvent::HandoverRequested { .. })) => handover_seen = true,
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
    drop(queue);
    harness.close().await;
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
async fn autoplay_first_registered_track_plays_first_even_when_loaded_last(
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    const TRACK_SECS: f64 = 0.4;
    const QUIET_VALUE: f32 = 0.10;
    const LOUD_VALUE: f32 = 0.80;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            true,
        )))
        .await;

    let id_a = queue.register_for_test();
    let id_b = queue.register_for_test();

    harness
        .run(&queue, move |q| {
            q.complete_load_for_test(id_b, make_resource("b", TRACK_SECS, constant_loud))
        })
        .await;
    harness
        .run(&queue, move |q| {
            q.complete_load_for_test(id_a, make_resource("a", TRACK_SECS, constant_quiet))
        })
        .await;

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("autoplay must start producing audio without an explicit select");
    let track_a_frames =
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * TRACK_SECS).unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;

    let mean_first = mean_abs_window(&pcm, onset + track_a_frames / 4, window)
        .expect("window inside the first audible track fits");
    let track_second_probe = onset + track_a_frames + track_a_frames / 4;
    let mean_second = mean_abs_window(&pcm, track_second_probe, window)
        .expect("window inside the second audible track fits");

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
    drop(queue);
    harness.close().await;
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
async fn cf_zero_replay_after_full_playthrough_still_advances(
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;

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

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("first select track A");
    let _first_pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;
    assert_eq!(
        queue.current_index(),
        Some(1),
        "first playthrough must reach track B"
    );

    queue.supply_test_resource_for_respawn(id_a, make_resource("a2", TRACK_SECS, constant_quiet));
    queue.supply_test_resource_for_respawn(id_b, make_resource("b2", TRACK_SECS, constant_loud));

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("second select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005).expect("track A must produce non-silence on replay");
    let track_a_frames =
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * TRACK_SECS).unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;

    let mean_a =
        mean_abs_window(&pcm, onset + track_a_frames / 4, window).expect("track A mid window fits");
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
    drop(queue);
    harness.close().await;
}

/// When the last track finishes, the live playback snapshot must become inactive
/// so the UI sees a stopped state even though transport intent remains unchanged.
#[kithara::test(tokio)]
async fn queue_stops_live_playback_when_last_track_ends(constant_three: &'static [u8]) {
    use kithara::{
        events::{Event, QueueEvent},
        platform::tokio::sync::broadcast::error::TryRecvError,
    };

    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;
    let mut rx = queue.subscribe();

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_three))
        })
        .await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let mut saw_queue_ended = false;
    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, |q| q.tick()).await;
        let _ = harness.render(BLOCK_FRAMES).await;
        loop {
            match rx.try_recv().map(|env| env.event) {
                Ok(Event::Queue(QueueEvent::QueueEnded)) => saw_queue_ended = true,
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if saw_queue_ended {
            for _ in 0..4 {
                let _ = harness.run(&queue, |q| q.tick()).await;
                let _ = harness.render(BLOCK_FRAMES).await;
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
        "live playback must stop after the last EOF"
    );
    drop(queue);
    harness.close().await;
}

/// Regression: `PrefetchRequested` can arrive before `current_index` is
/// written on autoplay start. If `peek_next` defaults `None` to `Some(0)`,
/// the prefetch handler arms slot 0 against the already-playing decoder.
#[kithara::test(tokio)]
async fn autoplay_first_track_does_not_self_arm_and_kill_its_own_decoder(
    constant_three: &'static [u8],
) {
    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            true,
        )))
        .await;

    let _id = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("solo", TRACK_SECS, constant_three))
        })
        .await;

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset =
        first_onset_frame(&pcm, 0.005).expect("autoplay'd track must produce audible samples");
    let track_frames =
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * TRACK_SECS).unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;

    let mean_mid = mean_abs_window(&pcm, onset + track_frames / 4, window)
        .expect("mid window inside the track fits");

    assert!(
        mean_mid > 0.005,
        "no signal mid-playback (mean={mean_mid}) — decoder likely self-armed"
    );

    assert_eq!(
        queue.current_index(),
        Some(0),
        "current_index must stay on the only track"
    );
    drop(queue);
    harness.close().await;
}

/// A queue played straight through has to let its middle track be heard.
///
/// Committing an advance moves the queue's cursor at once, while the track
/// just left keeps leading the mix until the engine hands over. The remaining
/// playtime on offer in that window belongs to the outgoing track; paired with
/// the incoming track's identity it reads as "this track is about to end" on
/// the incoming track's very first tick, and the queue advances straight past
/// it. Two tracks cannot show this — there is no successor left to jump to, so
/// the sibling crossfade test above stays green while a playlist skips.
#[kithara::test(tokio)]
async fn a_middle_track_is_heard_in_the_middle_of_its_own_span(
    constant_quiet: &'static [u8],
    constant_loud: &'static [u8],
    constant_four: &'static [u8],
) {
    const TRACK_SECS: f64 = 1.5;
    const CROSSFADE_SECS: f32 = 0.3;
    const LEVEL_A: f32 = 0.10;
    const LEVEL_B: f32 = 0.80;
    const LEVEL_C: f32 = 0.40;

    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(with_autoplay(
            QueueConfig::builder().player(harness.take_player()).build(),
            false,
        )))
        .await;

    let id_a = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("a", TRACK_SECS, constant_quiet))
        })
        .await;
    let _ = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("b", TRACK_SECS, constant_loud))
        })
        .await;
    let _ = harness
        .run(&queue, move |q| {
            q.insert_loaded_for_test(make_resource("c", TRACK_SECS, constant_four))
        })
        .await;
    // The app starts a catalog row exactly this way, with no fade into the
    // first track, and it is the arrangement that leaves the engine's own
    // handover trigger disarmed for that track.
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence within the render budget");
    // A track is left one crossfade before its own end, so that is how far
    // apart the seams stand and how long each track owns the output.
    let stride_frames = num_traits::cast::<f64, usize>(
        (TRACK_SECS - f64::from(CROSSFADE_SECS)) * f64::from(SAMPLE_RATE),
    )
    .unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;
    let middle_of = |index: usize| onset + stride_frames * index + stride_frames / 2;

    let mean_a =
        mean_abs_window(&pcm, middle_of(0), window).expect("track A's own span fits the take");
    let mean_b =
        mean_abs_window(&pcm, middle_of(1), window).expect("track B's own span fits the take");
    let mean_c =
        mean_abs_window(&pcm, middle_of(2), window).expect("track C's own span fits the take");

    assert!(
        mean_a > 0.005,
        "track A produced no audible signal: mean_a={mean_a}"
    );

    // Levels are compared as ratios against track A's, so whatever gain the
    // host applies cancels and only the identity of the track being heard is
    // left to decide the assertion.
    let ratio_b = mean_b / mean_a.max(f32::EPSILON);
    let expected_b = LEVEL_B / LEVEL_A;
    let expected_c = LEVEL_C / LEVEL_A;
    assert!(
        (ratio_b - expected_b).abs() < (ratio_b - expected_c).abs(),
        "the middle of track B's span is track C: the queue left B before it played. \
         ratio={ratio_b}, B={expected_b}, C={expected_c} \
         (mean_a={mean_a}, mean_b={mean_b})"
    );

    let ratio_c = mean_c / mean_a.max(f32::EPSILON);
    assert!(
        (ratio_c - expected_c).abs() < (ratio_c - expected_b).abs(),
        "the middle of track C's span is not track C: \
         ratio={ratio_c}, C={expected_c}, B={expected_b} \
         (mean_a={mean_a}, mean_c={mean_c})"
    );
    drop(queue);
    harness.close().await;
}
