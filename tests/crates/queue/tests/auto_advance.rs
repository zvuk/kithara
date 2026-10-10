#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    self,
    events::{EventReceiver, TrackId},
    platform::{sync::Arc, time::Duration},
    play::PlayError,
    queue::{
        ActionAtItemEnd, AdvanceReason, Queue, QueueConfig, QueueControl, QueueError, QueueEvent,
        RepeatMode, TrackStatus, Transition,
    },
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper,
    event::TestEvent,
    offline::{
        LOCAL_LOAD_DEADLINE, OfflinePlayer, OfflinePlayerOptions, append_loaded, asset_source,
    },
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::{
    assets,
    signal::{deinterleave_left, max_silence_run},
};
use num_traits::AsPrimitive;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const MAX_BLOCKS: usize = 1024;

fn queue_config(harness: &OfflinePlayer, duration: f32) -> QueueConfig<TestPools> {
    QueueConfig::builder()
        .prep(harness.resource_prep().clone())
        .settings(
            kithara::queue::QueueSettings::builder()
                .crossfade(kithara::play::CrossfadeSettings {
                    duration,
                    ..kithara::play::CrossfadeSettings::default()
                })
                .build(),
        )
        .build()
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
    let len: f32 = window.len().as_();
    Some(sum / len)
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
    harness: &OfflinePlayer,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::new();
    for _ in 0..block_budget {
        let _ = harness.run(queue, QueueControl::tick).await;
        let block = harness.render(BLOCK_FRAMES).await;
        pcm.extend(block);
    }
    pcm
}

#[kithara::test(tokio)]
async fn crossfade_started_requires_a_live_predecessor() {
    const CROSSFADE_SECS: f32 = 0.2;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, CROSSFADE_SECS)))
        .await;
    let initial = assets::constant_wav_three_0_2s();
    let id = append_loaded(&harness, &queue, &initial).await;
    let mut receiver = queue.subscribe();

    harness
        .run(&queue, move |q| q.select(id, Transition::Crossfade))
        .await
        .expect("select initial track");

    while let Ok(envelope) = receiver.try_recv() {
        assert!(
            !matches!(
                envelope.event,
                TestEvent::Queue(QueueEvent::CrossfadeStarted { .. })
            ),
            "a cold select cannot crossfade from an idle player"
        );
    }

    let mut saw_playing = false;
    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, QueueControl::tick).await;
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
        .run(&queue, QueueControl::tick)
        .await
        .expect("process predecessor EOF");

    let successor_wav = assets::constant_wav_three_1s();
    let successor = append_loaded(&harness, &queue, &successor_wav).await;
    let mut receiver = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(successor, Transition::Crossfade))
        .await
        .expect("select successor after EOF");

    while let Ok(envelope) = receiver.try_recv() {
        assert!(
            !matches!(
                envelope.event,
                TestEvent::Queue(QueueEvent::CrossfadeStarted { .. })
            ),
            "a completed predecessor cannot start a crossfade"
        );
    }
    drop(queue);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn repeat_one_natural_advance_keeps_current_track() {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;
    let one = assets::constant_wav_three_1s();
    let id = append_loaded(&harness, &queue, &one).await;
    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select repeat-one track");
    let mut receiver: EventReceiver<QueueEvent> = queue.subscribe();
    harness.run(&queue, |q| q.set_repeat(RepeatMode::One)).await;

    assert!(matches!(
        receiver.try_recv().map(|envelope| envelope.event),
        Ok(QueueEvent::RepeatModeChanged {
            mode: kithara::queue::QueueRepeatMode::One,
        })
    ));
    let _ = render_loop(&queue, &harness, MAX_BLOCKS).await;
    assert_eq!(queue.current().map(|entry| entry.id), Some(id));
    assert!(
        queue.is_playing(),
        "repeat one must restart after natural EOF"
    );
    drop(queue);
    harness.close().await;
}

/// Picking a track is itself a request to hear it. The queue starts stopped
/// and first-load autoplay is off, so the only thing that can open the
/// transport is the explicit selection.
#[kithara::test(tokio)]
async fn selecting_a_loaded_track_starts_playback_from_a_stopped_transport() {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(
            QueueConfig::builder()
                .prep(harness.resource_prep().clone())
                .build(),
        ))
        .await;
    let one = assets::constant_wav_three_1s();
    let id = append_loaded(&harness, &queue, &one).await;

    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select the loaded track");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;
    assert!(
        first_onset_frame(&pcm, 0.005).is_some(),
        "an explicit selection must start playback from a stopped transport"
    );
    drop(queue);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn repeat_all_natural_advance_wraps_last_track_to_first() {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;
    let first_wav = assets::constant_wav_two_1s();
    let first = append_loaded(&harness, &queue, &first_wav).await;
    let last_wav = assets::constant_wav_loud_1s();
    let last = append_loaded(&harness, &queue, &last_wav).await;
    harness
        .run(&queue, move |q| q.select(last, Transition::None))
        .await
        .expect("select last repeat-all track");
    let mut receiver: EventReceiver<QueueEvent> = queue.subscribe();
    harness.run(&queue, |q| q.set_repeat(RepeatMode::All)).await;

    assert!(matches!(
        receiver.try_recv().map(|envelope| envelope.event),
        Ok(QueueEvent::RepeatModeChanged {
            mode: kithara::queue::QueueRepeatMode::All,
        })
    ));
    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, QueueControl::tick).await;
        let _ = harness.render(BLOCK_FRAMES).await;
        if queue.current().is_some_and(|entry| entry.id == first) {
            break;
        }
    }
    assert_eq!(queue.current().map(|entry| entry.id), Some(first));
    drop(queue);
    harness.close().await;
}

/// cf=0: queue.tick must drive `process_notifications`, the audio thread
/// arena handover at EOF promotes the armed next track, and the second
/// track's PCM signal must replace the first one's.
#[kithara::test(tokio)]
async fn cf_zero_queue_tick_advances_to_second_track_audio() {
    const TRACK_SECS: f64 = 0.4;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;

    let a = assets::constant_wav_quiet_0_4s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_0_4s();
    let _ = append_loaded(&harness, &queue, &b).await;
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

/// The Host ticks the decks it holds: a queue moves on to its next track,
/// announces it and sounds it while the Host renders, with no caller ticking
/// the queue.
#[kithara::test(tokio)]
async fn a_queue_the_host_holds_moves_on_with_no_caller_ticking_it() {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;
    let a = assets::constant_wav_quiet_0_4s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_0_4s();
    let id_b = append_loaded(&harness, &queue, &b).await;
    let mut events = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let mut announced = false;
    let mut pcm = Vec::new();
    for _ in 0..MAX_BLOCKS {
        pcm.extend(harness.render(BLOCK_FRAMES).await);
        while let Ok(envelope) = events.try_recv() {
            announced |= matches!(
                envelope.event,
                TestEvent::Queue(QueueEvent::CurrentTrackChanged { id }) if id == Some(id_b)
            );
        }
        if announced {
            break;
        }
    }

    assert!(
        announced,
        "the queue announces track B with no caller ticking it"
    );
    let onset = first_onset_frame(&pcm, 0.005).expect("track A sounds");
    let loud = pcm
        .chunks_exact(usize::from(CHANNELS))
        .skip(onset)
        .any(|frame| frame.iter().any(|sample| sample.abs() > 0.5));
    assert!(
        loud,
        "track B sounds after track A with no caller ticking the queue"
    );
    assert_eq!(queue.current_index(), Some(1));
    drop(queue);
    harness.close().await;
}

/// A gapless queue hands the next track to the deck before the current one
/// ends, chained behind it, so the next track's first frame follows the
/// current one's last with no silence between them, and it becomes current
/// by the current track's natural end, never by a crossfade.
#[kithara::test(tokio)]
async fn a_gapless_queue_meets_its_next_track_without_a_gap() {
    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;
    const SILENCE: f32 = 0.005;
    /// Between A's level (≈0.1) and B's (≈0.8).
    const TRACK_B_LEVEL: f32 = 0.45;
    const STITCH_TOLERANCE_FRAMES: usize = BLOCK_FRAMES / 8;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;

    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let id_b = append_loaded(&harness, &queue, &b).await;
    let mut events = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    // Drain the bus every block so a long render cannot lag the receiver.
    let mut pcm = Vec::new();
    let mut advances_to_b = Vec::new();
    let mut crossfades = 0_usize;
    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, QueueControl::tick).await;
        pcm.extend(harness.render(BLOCK_FRAMES).await);
        while let Ok(envelope) = events.try_recv() {
            match envelope.event {
                TestEvent::Queue(QueueEvent::CurrentTrackAdvance {
                    id: Some(id),
                    reason,
                }) if id == id_b => advances_to_b.push(reason),
                TestEvent::Queue(QueueEvent::CrossfadeStarted { .. }) => crossfades += 1,
                _ => {}
            }
        }
    }

    let onset = first_onset_frame(&pcm, SILENCE)
        .expect("track A must produce non-silence within the render budget");
    let left = deinterleave_left(&pcm, usize::from(CHANNELS));
    let a_end = onset + TRACK_FRAMES;

    let gap = max_silence_run(&left, onset, a_end + TRACK_FRAMES / 3, SILENCE);
    assert_eq!(
        gap, 0,
        "B must follow A with no silence between them: {gap} silent frames \
         (onset={onset}, A ends at {a_end})"
    );

    let rise = left[onset..]
        .iter()
        .position(|sample| sample.abs() > TRACK_B_LEVEL)
        .map(|offset| onset + offset)
        .expect("track B must be heard within the render budget");
    assert!(
        rise.abs_diff(a_end) <= STITCH_TOLERANCE_FRAMES,
        "B's first frame must be the one after A's last: rise={rise}, A ends at {a_end}"
    );
    let b_span = left[rise..]
        .iter()
        .take_while(|sample| sample.abs() > TRACK_B_LEVEL)
        .count();
    assert!(
        b_span.abs_diff(TRACK_FRAMES) <= BLOCK_FRAMES,
        "B must play once, whole, from its start: {b_span} frames, expected ≈{TRACK_FRAMES}"
    );

    assert_eq!(
        advances_to_b,
        [AdvanceReason::NaturalEof],
        "B becomes current once, by A's natural end"
    );
    assert_eq!(crossfades, 0, "a gapless transition starts no crossfade");
    assert_eq!(
        queue.current_index(),
        Some(1),
        "queue.current_index must follow the deck to track B"
    );
    drop(queue);
    harness.close().await;
}

/// A successor the deck has no room for is not armed: its resource went with
/// the attach the deck refused, so the queue reloads it, and it still meets
/// the current track without a gap.
#[kithara::test(tokio)]
async fn a_successor_the_deck_had_no_room_for_reloads_and_meets_its_predecessor() {
    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;
    const SILENCE: f32 = 0.005;
    /// Between A's level (≈0.1) and B's (≈0.8).
    const TRACK_B_LEVEL: f32 = 0.45;
    const STITCH_TOLERANCE_FRAMES: usize = BLOCK_FRAMES / 8;
    const LEAD_SECS: f32 = 0.5;
    /// Blocks A plays before its end comes inside the lead: ≈0.93 s of its
    /// 1.5 s.
    const OUTSIDE_LEAD_BLOCKS: usize = 80;
    /// More batches than the deck's command ring holds.
    const FLOOD: usize = 64;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let config = QueueConfig::builder()
        .prep(harness.resource_prep().clone())
        .settings(
            kithara::queue::QueueSettings::builder()
                .crossfade(kithara::play::CrossfadeSettings {
                    duration: 0.0,
                    ..kithara::play::CrossfadeSettings::default()
                })
                .build(),
        )
        .preload_lead(Duration::from_secs_f32(LEAD_SECS))
        .build();
    let queue = harness.insert_control(Queue::new(config)).await;
    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let id_b = append_loaded(&harness, &queue, &b).await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    // The Host ticks the deck ahead of every block it renders; A is far from
    // its end for these, so nothing is armed yet.
    let mut pcm = Vec::new();
    for _ in 0..OUTSIDE_LEAD_BLOCKS {
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }
    // Fill the deck's command ring ahead of each block from here, so the tick
    // that first finds A's end inside the lead has no room to arm B. An arm
    // that went through would leave B loaded and A ending.
    while queue.track(id_b).map(|entry| entry.status) != Some(TrackStatus::Consumed) {
        assert!(
            pcm.len() < TRACK_FRAMES * usize::from(CHANNELS),
            "B's resource went with the attach the deck refused, so B must reload \
             before A ends"
        );
        let refused = harness
            .run(&queue, |q| (0..FLOOD).find_map(|_| q.set_volume(1.0).err()))
            .await;
        assert!(
            matches!(refused, Some(QueueError::Play(PlayError::Full(_)))),
            "the flood fills the deck's ring, so it refuses a volume, not {refused:?}"
        );
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }

    // Subscribe only now: the refused arm already published B's `Consumed`,
    // which the wait would take for a finished reload. The tick ahead of the
    // next block starts that reload.
    let mut events = queue.subscribe();
    pcm.extend(harness.render(BLOCK_FRAMES).await);
    wait_for_loader_done_event(&mut events, &queue, id_b, LOCAL_LOAD_DEADLINE)
        .await
        .expect("track B reloads");
    for _ in 0..TRACK_FRAMES / BLOCK_FRAMES {
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }

    let onset = first_onset_frame(&pcm, SILENCE)
        .expect("track A must produce non-silence within the render budget");
    let left = deinterleave_left(&pcm, usize::from(CHANNELS));
    let a_end = onset + TRACK_FRAMES;
    let gap = max_silence_run(&left, onset, a_end + TRACK_FRAMES / 3, SILENCE);
    assert_eq!(
        gap, 0,
        "B must follow A with no silence between them: {gap} silent frames \
         (onset={onset}, A ends at {a_end})"
    );
    let rise = left[onset..]
        .iter()
        .position(|sample| sample.abs() > TRACK_B_LEVEL)
        .map(|offset| onset + offset)
        .expect("track B must be heard within the render budget");
    assert!(
        rise.abs_diff(a_end) <= STITCH_TOLERANCE_FRAMES,
        "B's first frame must be the one after A's last: rise={rise}, A ends at {a_end}"
    );
    drop(queue);
    harness.close().await;
}

/// Blocks rendered with ticks after a gapless queue starts its first track,
/// long enough for the queue to arm the next one behind it.
const ARMING_BLOCKS: usize = 16;

/// A gapless queue playing `constant_wav_quiet_1_5s` with
/// `constant_wav_loud_1_5s` armed behind it.
async fn gapless_queue_with_an_armed_successor()
-> (OfflinePlayer, QueueControl<TestPools>, TrackId, TrackId) {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;
    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let id_b = append_loaded(&harness, &queue, &b).await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    let _ = render_loop(&queue, &harness, ARMING_BLOCKS).await;
    (harness, queue, id_a, id_b)
}

/// Renders blocks until the one in which the deck stitches in the loud
/// successor behind the quiet track it plays. The Host ticks the queue only
/// ahead of a block, so no tick has read the stitch yet.
async fn render_through_the_stitch(harness: &OfflinePlayer, block_budget: usize) {
    /// Between `constant_wav_quiet_1_5s` (≈0.1) and `constant_wav_loud_1_5s`
    /// (≈0.8).
    const LOUD_LEVEL: f32 = 0.45;

    for _ in 0..block_budget {
        let block = harness.render(BLOCK_FRAMES).await;
        if block.iter().any(|sample| sample.abs() > LOUD_LEVEL) {
            return;
        }
    }
    panic!("the deck must stitch in its successor within {block_budget} blocks");
}

/// Asking the queue to pause at the current track's end takes effect at
/// once: the deck stops at that end even when no tick comes before it,
/// instead of stitching in the gapless successor armed behind it.
#[kithara::test(tokio)]
async fn pausing_at_the_end_takes_back_the_armed_successor_at_once() {
    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;
    /// Between A's level (≈0.1) and B's (≈0.8).
    const TRACK_B_LEVEL: f32 = 0.45;

    let (harness, queue, _, id_b) = gapless_queue_with_an_armed_successor().await;
    harness
        .run(&queue, |q| q.set_action_at_item_end(ActionAtItemEnd::Pause))
        .await;
    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Consumed),
        "the call itself takes B off the deck, ahead of any tick"
    );

    let mut pcm = Vec::new();
    for _ in 0..TRACK_FRAMES / BLOCK_FRAMES + ARMING_BLOCKS {
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }
    let loudest = pcm
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    assert!(
        loudest < TRACK_B_LEVEL,
        "the deck must stop at A's end, not play B: peak {loudest}"
    );
    drop(queue);
    harness.close().await;
}

/// The deck hands a gapless successor over on its own. A pause before the
/// next tick gates the end report, so the queue must follow the deck's
/// announcement of the successor, not wait for the end.
#[kithara::test(tokio)]
async fn a_pause_after_the_deck_stitches_in_the_successor_leaves_the_queue_on_it() {
    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;

    let (harness, queue, id_a, id_b) = gapless_queue_with_an_armed_successor().await;
    render_through_the_stitch(&harness, TRACK_FRAMES / BLOCK_FRAMES).await;
    assert_eq!(
        queue.current().map(|entry| entry.id),
        Some(id_a),
        "no tick has read the stitch before the pause"
    );
    harness.run(&queue, QueueControl::pause).await;
    let _ = harness.run(&queue, QueueControl::tick).await;

    assert_eq!(
        queue.current().map(|entry| entry.id),
        Some(id_b),
        "the queue's current track is the one the deck stitched in"
    );
    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Consumed),
        "the deck holds the stitched successor, so the queue has consumed it"
    );
    drop(queue);
    harness.close().await;
}

/// Removing the successor the deck already stitched in, before a tick reads
/// the stitch, leaves the queue nothing to follow it to: the end of the track
/// before it advances to the next track the queue holds.
#[kithara::test(tokio)]
async fn removing_a_successor_the_deck_already_stitched_in_advances_past_it() {
    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;

    let (harness, queue, id_a, id_b) = gapless_queue_with_an_armed_successor().await;
    let c = assets::constant_wav_four_1_5s();
    let id_c = append_loaded(&harness, &queue, &c).await;
    render_through_the_stitch(&harness, TRACK_FRAMES / BLOCK_FRAMES).await;
    assert_eq!(
        queue.current().map(|entry| entry.id),
        Some(id_a),
        "no tick has read the stitch before the removal"
    );
    harness
        .run(&queue, move |q| q.remove(id_b))
        .await
        .expect("remove track B");
    let _ = harness.run(&queue, QueueControl::tick).await;

    assert_eq!(
        queue.current().map(|entry| entry.id),
        Some(id_c),
        "the queue moves past the removed B to C"
    );
    drop(queue);
    harness.close().await;
}

/// A track inserted after the playing one follows it even when no tick comes
/// between the insert and the end: the insert itself takes the successor it
/// displaced off the deck.
#[kithara::test(tokio)]
async fn a_track_inserted_after_the_playing_one_follows_it_without_a_tick_before_the_end() {
    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;

    let (harness, queue, id_a, id_b) = gapless_queue_with_an_armed_successor().await;
    let c = assets::constant_wav_four_1_5s();
    let mut events = queue.subscribe();
    let id_c = harness
        .run(&queue, move |q| q.insert(asset_source(&c), Some(id_a)))
        .await
        .expect("insert track C after A");
    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Consumed),
        "the insert itself takes the displaced B off the deck, ahead of any tick"
    );
    wait_for_loader_done_event(&mut events, &queue, id_c, LOCAL_LOAD_DEADLINE)
        .await
        .expect("track C loads");
    for _ in 0..TRACK_FRAMES / BLOCK_FRAMES {
        let _ = harness.render(BLOCK_FRAMES).await;
    }
    let _ = harness.run(&queue, QueueControl::tick).await;

    assert_eq!(
        queue.current().map(|entry| entry.id),
        Some(id_c),
        "C follows A"
    );
    drop(queue);
    harness.close().await;
}

/// Selecting the track that already plays leaves the successor armed behind
/// it where it is.
#[kithara::test(tokio)]
async fn reselecting_the_playing_track_keeps_its_successor_armed() {
    let (harness, queue, id_a, id_b) = gapless_queue_with_an_armed_successor().await;
    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Loaded),
        "the armed successor keeps its loaded status"
    );

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select track A");

    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Loaded),
        "re-selecting A must not take B off the deck"
    );
    drop(queue);
    harness.close().await;
}

/// A track inserted after the playing one while a successor is armed is the
/// one that follows it; the successor it displaced plays after it.
#[kithara::test(tokio)]
async fn a_track_inserted_after_the_playing_one_follows_it_before_the_armed_successor() {
    use kithara::{platform::tokio::sync::broadcast::error::TryRecvError, queue::QueueEvent};

    /// `constant_wav_*_1_5s`: 1.5 s at 44.1 kHz.
    const TRACK_FRAMES: usize = 66_150;
    /// Between A's level (≈0.1) and C's (≈0.4).
    const AFTER_A: f32 = 0.25;
    /// Between C's level (≈0.4) and B's (≈0.8).
    const AFTER_C: f32 = 0.6;
    const TRACK_C_LEVEL: f32 = 0.4;

    let (harness, queue, id_a, id_b) = gapless_queue_with_an_armed_successor().await;
    let c = assets::constant_wav_four_1_5s();
    let mut events = queue.subscribe();
    let id_c = harness
        .run(&queue, move |q| q.insert(asset_source(&c), Some(id_a)))
        .await
        .expect("insert track C after A");
    wait_for_loader_done_event(&mut events, &queue, id_c, LOCAL_LOAD_DEADLINE)
        .await
        .expect("track C loads");

    // Taking B off the deck spent its resource, so B reloads once C nears its
    // end. Offline rendering outruns a load, so the render holds for it the
    // way real time would.
    let mut pcm = Vec::new();
    let mut reloading = false;
    for _ in 0..2 * TRACK_FRAMES / BLOCK_FRAMES {
        pcm.extend(render_loop(&queue, &harness, 1).await);
        loop {
            match events.try_recv().map(|envelope| envelope.event) {
                Ok(TestEvent::Queue(QueueEvent::TrackStatusChanged {
                    id,
                    status: TrackStatus::Pending,
                })) if id == id_b => reloading = true,
                Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        if reloading {
            break;
        }
    }
    assert!(reloading, "the displaced B reloads behind C");
    wait_for_loader_done_event(&mut events, &queue, id_b, LOCAL_LOAD_DEADLINE)
        .await
        .expect("track B reloads");
    pcm.extend(render_loop(&queue, &harness, 2 * TRACK_FRAMES / BLOCK_FRAMES).await);
    let after_a = first_onset_frame(&pcm, AFTER_A).expect("a louder track follows A");
    let follower = mean_abs_window(&pcm, after_a + TRACK_FRAMES / 4, TRACK_FRAMES / 4)
        .expect("the follower's window fits");
    assert!(
        (follower - TRACK_C_LEVEL).abs() < 0.05,
        "C must follow A: level {follower}"
    );
    let b_after_a = first_onset_frame(&pcm[after_a * usize::from(CHANNELS)..], AFTER_C)
        .expect("B plays after C");
    assert!(
        b_after_a >= TRACK_FRAMES * 9 / 10,
        "the displaced B must not sound before C has played its length: B at {b_after_a} frames after A"
    );
    drop(queue);
    harness.close().await;
}

/// cf>0: queue.tick sees the crossfade window open and commits the
/// successor, the two tracks overlap in the crossfade window and PCM mid-track-B
/// must show track B's value.
#[kithara::test(tokio)]
async fn cf_nonzero_queue_tick_crossfades_to_second_track_audio() {
    const TRACK_SECS: f64 = 1.5;
    const CROSSFADE_SECS: f32 = 0.3;
    const TRACK_A_VALUE: f32 = 0.10;
    const TRACK_B_VALUE: f32 = 0.80;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, CROSSFADE_SECS)))
        .await;

    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let _ = append_loaded(&harness, &queue, &b).await;
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

/// A crossfade lasts its duration in session time at any playback speed, so the
/// queue starts it that long before the outgoing track's end on the session clock:
/// at speed 2 a 1.5 s track ends 0.75 s after its onset, and the incoming track
/// rises from 0.75 s − d on, not from where `d` media seconds remain.
#[kithara::test(tokio)]
async fn a_crossfade_at_double_speed_starts_its_duration_before_the_outgoing_end() {
    const TRACK_SECS: f64 = 1.5;
    const SPEED: f32 = 2.0;
    const CROSSFADE_SECS: f32 = 0.3;
    const TRACK_A_VALUE: f32 = 0.10;
    /// Two blocks of tick lateness, plus the fade's first 5 % before the incoming
    /// track clears the outgoing level by half.
    const TOLERANCE_FRAMES: usize = 2 * BLOCK_FRAMES + 662;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, CROSSFADE_SECS)))
        .await;

    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let _ = append_loaded(&harness, &queue, &b).await;
    harness
        .run(&queue, |q| q.set_default_rate(SPEED))
        .await
        .expect("a finite rate is accepted");
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("track A must produce non-silence within the render budget");
    let session_frames = |secs: f64| {
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * secs).unwrap_or(usize::MAX)
    };
    let a_end = onset + session_frames(TRACK_SECS / f64::from(SPEED));
    let fade_start = a_end - session_frames(f64::from(CROSSFADE_SECS));
    let channels = usize::from(CHANNELS);
    let rise = pcm
        .chunks_exact(channels)
        .skip(onset)
        .position(|frame| frame.iter().any(|s| s.abs() > TRACK_A_VALUE * 1.5))
        .map(|offset| onset + offset)
        .expect("the incoming track must rise above the outgoing level");

    assert!(
        rise.abs_diff(fade_start) <= TOLERANCE_FRAMES,
        "the incoming track must rise from {CROSSFADE_SECS} s of session time before the \
         outgoing end: expected frame ≈ {fade_start}, got {rise} (onset {onset}, end {a_end})"
    );
    drop(queue);
    harness.close().await;
}

/// Sanity guard: if `Queue::tick` regresses to skipping
/// `process_notifications`, this test must fail. The audio thread's end of
/// the first track reaches the bus during a cf>0 cycle — purely event-level,
/// but pinned to the real `Queue::tick` path.
#[kithara::test(tokio)]
async fn queue_tick_pumps_audio_thread_notifications_to_bus() {
    use kithara::{platform::tokio::sync::broadcast::error::TryRecvError, play::PlayerEvent};

    const CROSSFADE_SECS: f32 = 0.2;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, CROSSFADE_SECS)))
        .await;
    let mut rx = queue.subscribe();

    let a = assets::constant_wav_quiet_1s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1s();
    let _ = append_loaded(&harness, &queue, &b).await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let mut item_end_seen = false;

    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, QueueControl::tick).await;
        let _ = harness.render(BLOCK_FRAMES).await;

        loop {
            match rx.try_recv().map(|env| env.event) {
                Ok(TestEvent::Player(PlayerEvent::ItemDidPlayToEnd { .. })) => item_end_seen = true,
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if item_end_seen {
            break;
        }
    }

    assert!(
        item_end_seen,
        "ItemDidPlayToEnd must reach the bus via Queue::tick → process_notifications"
    );
    drop(queue);
    harness.close().await;
}

/// The lead is how long a reload has before the successor is needed, and
/// that time passes on the session clock: at double speed a consumed
/// successor reloads when the outgoing track has `lead` session seconds
/// left, which is twice the lead in media seconds.
#[kithara::test(tokio)]
async fn a_consumed_successor_reloads_its_lead_in_session_time_before_the_end() {
    const TRACK_SECS: f64 = 1.5;
    const SPEED: f32 = 2.0;
    const LEAD_SECS: f32 = 0.5;
    const TOLERANCE_SECS: f64 = 0.1;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let config = QueueConfig::builder()
        .prep(harness.resource_prep().clone())
        .settings(
            kithara::queue::QueueSettings::builder()
                .crossfade(kithara::play::CrossfadeSettings {
                    duration: 0.0,
                    ..kithara::play::CrossfadeSettings::default()
                })
                .build(),
        )
        .preload_lead(Duration::from_secs_f32(LEAD_SECS))
        .build();
    let queue = harness.insert_control(Queue::new(config)).await;

    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let id_b = append_loaded(&harness, &queue, &b).await;
    harness
        .run(&queue, move |q| q.set_default_rate(SPEED))
        .await
        .expect("a finite rate is accepted");

    harness
        .run(&queue, move |q| q.select(id_b, Transition::None))
        .await
        .expect("select track B");
    let _ = render_loop(&queue, &harness, 8).await;
    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Consumed),
        "playing B consumes its resource"
    );

    // The queue loaded A paused on its first append, and B replaced it, so
    // selecting A reloads it; measure only once A is the track playing.
    let mut a_events = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    wait_for_loader_done_event(&mut a_events, &queue, id_a, LOCAL_LOAD_DEADLINE)
        .await
        .expect("track A reloads");
    let mut reload = None;
    for block in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, QueueControl::tick).await;
        let status = queue.track(id_b).map(|entry| entry.status);
        if status != Some(TrackStatus::Consumed) {
            reload = Some((block, status, queue.current_index(), queue.playback_view()));
            break;
        }
        let _ = harness.render(BLOCK_FRAMES).await;
    }

    let (block, status, current, view) = reload.expect("B reloads while A still plays");
    assert_eq!(current, Some(0), "B reloads ahead of A, the track playing");
    let pos = view.position.unwrap_or_else(|| {
        panic!("A has a position when B reloads (block {block}, B {status:?}, view {view:?})")
    });
    let expected = TRACK_SECS - f64::from(LEAD_SECS * SPEED);
    assert!(
        (pos - expected).abs() <= TOLERANCE_SECS,
        "B must reload with {LEAD_SECS}s of session time left in A: \
         expected A at ≈{expected}s media, got {pos}s"
    );
    drop(queue);
    harness.close().await;
}

/// Replay regression: after a full cf=0 playthrough every track is
/// `Consumed`. A second pass over the same queue must still
/// auto-advance — i.e. the tick must respawn the `Consumed` next-track
/// via the loader path so the advance has a resource again. Before the
/// fix, the queue stopped after the first track on every replay.
///
/// Drives the full production code path: the second `select` of track A
/// hits the `Consumed` branch in `Queue::select` (which respawns via
/// `spawn_apply_after_load`), then within the prefetch lead of A's end
/// the tick respawns `Consumed` track B. We pre-supply fresh
/// `Resource`s to the loader so spawn completes synthetically, mirroring
/// what a real network loader would deliver on a replay.
#[kithara::test(tokio, flash(false))]
async fn cf_zero_replay_after_full_playthrough_still_advances() {
    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;

    let a = assets::constant_wav_quiet_0_4s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_0_4s();
    append_loaded(&harness, &queue, &b).await;

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

    let mut reload_events = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("second select track A");
    wait_for_loader_done_event(&mut reload_events, &queue, id_a, LOCAL_LOAD_DEADLINE)
        .await
        .expect("the fixture track loads");

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
async fn queue_stops_live_playback_when_last_track_ends() {
    use kithara::{platform::tokio::sync::broadcast::error::TryRecvError, queue::QueueEvent};

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, 0.0)))
        .await;
    let mut rx = queue.subscribe();

    let a = assets::constant_wav_three_0_4s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");

    let mut saw_queue_ended = false;
    for _ in 0..MAX_BLOCKS {
        let _ = harness.run(&queue, QueueControl::tick).await;
        let _ = harness.render(BLOCK_FRAMES).await;
        loop {
            match rx.try_recv().map(|env| env.event) {
                Ok(TestEvent::Queue(QueueEvent::QueueEnded)) => saw_queue_ended = true,
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        if saw_queue_ended {
            for _ in 0..4 {
                let _ = harness.run(&queue, QueueControl::tick).await;
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
async fn a_middle_track_is_heard_in_the_middle_of_its_own_span() {
    const TRACK_SECS: f64 = 1.5;
    const CROSSFADE_SECS: f32 = 0.3;
    const LEVEL_A: f32 = 0.10;
    const LEVEL_B: f32 = 0.80;
    const LEVEL_C: f32 = 0.40;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(CROSSFADE_SECS)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(queue_config(&harness, CROSSFADE_SECS)))
        .await;

    let a = assets::constant_wav_quiet_1_5s();
    let id_a = append_loaded(&harness, &queue, &a).await;
    let b = assets::constant_wav_loud_1_5s();
    let _ = append_loaded(&harness, &queue, &b).await;
    let c = assets::constant_wav_four_1_5s();
    let _ = append_loaded(&harness, &queue, &c).await;
    // The app starts a catalog row exactly this way, with no fade into the
    // first track.
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

async fn autoplay_queue(harness: &OfflinePlayer) -> QueueControl<TestPools> {
    harness
        .insert_control(Queue::new(
            QueueConfig::builder()
                .prep(harness.resource_prep().clone())
                .should_autoplay(true)
                .settings(
                    kithara::queue::QueueSettings::builder()
                        .crossfade(kithara::play::CrossfadeSettings {
                            duration: 0.0,
                            ..kithara::play::CrossfadeSettings::default()
                        })
                        .build(),
                )
                .build(),
        ))
        .await
}

/// Autoplay starts the first appended track even when its load finishes last,
/// then advances to the next one.
///
/// Track A is quiet and served whole after a delay, track B is loud and local,
/// so B is loaded first; if B preempted A the early window would carry B's level.
#[kithara::test(
    tokio,
    tracing("kithara_queue=debug,kithara_file=debug,kithara_storage=debug")
)]
async fn autoplay_first_appended_track_plays_first_even_when_loaded_last() {
    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = autoplay_queue(&harness).await;
    let mut events: EventReceiver<TestEvent> = queue.subscribe();

    let server = TestServerHelper::new().await;
    let bytes_a = assets::constant_wav_quiet_0_4s().bytes().to_vec();
    let source_a = server
        .register_behavior(FixtureBehavior {
            delivery: Delivery::Throttle {
                chunk: bytes_a.len(),
                delay_ms: 300,
            },
            content: Content::StaticBytes {
                bytes: Arc::new(bytes_a),
                content_type: Some("audio/wav"),
            },
        })
        .child_url("throttled-a.wav")
        .to_string();
    let source_b = asset_source(&assets::constant_wav_loud_0_4s());
    let (id_a, id_b) = harness
        .run(&queue, move |q| {
            (
                q.append(source_a).expect("append A"),
                q.append(source_b).expect("append B"),
            )
        })
        .await;
    wait_for_loader_done_event(&mut events, &queue, id_b, LOCAL_LOAD_DEADLINE)
        .await
        .expect("the fixture track loads");
    assert_ne!(
        queue.track(id_a).map(|entry| entry.status),
        Some(TrackStatus::Loaded),
        "the throttled first track must still be loading when the second loads"
    );
    wait_for_loader_done_event(&mut events, &queue, id_a, LOCAL_LOAD_DEADLINE)
        .await
        .expect("the fixture track loads");

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset = first_onset_frame(&pcm, 0.005)
        .expect("autoplay must start producing audio without an explicit select");
    let track_frames =
        num_traits::cast::<f64, usize>(f64::from(SAMPLE_RATE) * TRACK_SECS).unwrap_or(usize::MAX);
    let window = SAMPLE_RATE as usize / 8;
    let mean_first = mean_abs_window(&pcm, onset + track_frames / 4, window)
        .expect("window inside the first audible track fits");
    let mean_second = mean_abs_window(&pcm, onset + track_frames + track_frames / 4, window)
        .expect("window inside the second audible track fits");

    assert!(
        mean_second > mean_first * 4.0,
        "the quiet first-appended track A must play before the loud B: \
         mean_first={mean_first}, mean_second={mean_second}"
    );
    assert_eq!(
        queue.current_index(),
        Some(1),
        "after A finishes the queue must advance to B"
    );
    drop(queue);
    harness.close().await;
}

/// A short autoplayed track opens its prefetch lead at once; the reload it
/// prompts must not arm slot 0 against the decoder already playing it.
#[kithara::test(tokio)]
async fn autoplay_first_track_does_not_self_arm_and_kill_its_own_decoder() {
    const TRACK_SECS: f64 = 0.4;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = autoplay_queue(&harness).await;
    let solo = assets::constant_wav_three_0_4s();
    let _ = append_loaded(&harness, &queue, &solo).await;

    let pcm = render_loop(&queue, &harness, MAX_BLOCKS).await;

    let onset =
        first_onset_frame(&pcm, 0.005).expect("the autoplayed track must produce audible samples");
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
