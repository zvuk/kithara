#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    self,
    events::{EventReceiver, TrackId},
    platform::{
        sync::Arc,
        time::{self, Duration, Instant},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::PlayerEvent,
    queue::{AdvanceReason, QueueControl, QueueEvent, RepeatMode, TrackStatus, Transition},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper,
    event::TestEvent,
    offline::{
        OfflinePlayer, OfflinePlayerOptions, append_loaded, offline_queue_fixture,
        offline_queue_fixture_with_options,
    },
};
use kithara_test_fixtures::{
    assets,
    signal::{mean_abs, rms},
};
use num_traits::ToPrimitive;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;

/// One block at a device's cadence: the queue ticks as the app update loop
/// would, the Host renders the block, and the test clock waits out the rest of
/// the block's period. A device pulls no faster than it plays, so the worker
/// decodes at that cadence and a silent block is the pipeline's, not the
/// machine's.
async fn play_block(queue: &QueueControl<TestPools>, harness: &OfflinePlayer) -> Vec<f32> {
    let period = Duration::from_secs_f64(
        BLOCK_FRAMES.to_f64().expect("block frame count fits f64") / f64::from(SAMPLE_RATE),
    );
    let started = Instant::now();
    let _ = harness.run(queue, QueueControl::tick).await;
    let pcm = harness.render(BLOCK_FRAMES).await;
    time::sleep(period.saturating_sub(started.elapsed())).await;
    pcm
}

/// The advance a natural end asks for, named for the entry it lands on.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
async fn wait_for_eof_advance(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayer,
    events: &mut EventReceiver<QueueEvent>,
    id: TrackId,
) {
    let answered = time::timeout(__hang_detector.remaining(), async {
        loop {
            hang_tick!(format!(
                "repeat-one waits for natural EOF advance onto {id:?}"
            ));
            let envelope = match events.try_recv() {
                Ok(envelope) => envelope,
                Err(TryRecvError::Empty) => {
                    let _ = play_block(queue, harness).await;
                    continue;
                }
                Err(_) => return false,
            };
            if matches!(
                envelope.event,
                QueueEvent::CurrentTrackAdvance {
                    id: Some(seen),
                    reason: AdvanceReason::NaturalEof,
                } if seen == id
            ) {
                hang_reset!();
                return true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        answered,
        "repeat-one must answer the end of {id:?} with an advance onto it"
    );
}

/// Drive the queue until it reports `id` as its current item. Selecting a
/// consumed track spawns a fresh load and applies the stashed select only once
/// that load lands, so a fixed budget of renders started at `select` measures
/// the loader's latency instead of the switch. The statement travels from the
/// player through `tick`, so the wait has to render; subscribe before the
/// select, because the event is edge-triggered and never repeats.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
async fn wait_for_current_track(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayer,
    events: &mut EventReceiver<QueueEvent>,
    id: TrackId,
) {
    let landed = time::timeout(__hang_detector.remaining(), async {
        loop {
            hang_tick!(format!("queue waits for current track {id:?}"));
            while let Ok(envelope) = events.try_recv() {
                hang_tick!(format!("queue drains current track events for {id:?}"));
                if matches!(
                    envelope.event,
                    QueueEvent::CurrentTrackChanged { id: Some(seen) } if seen == id
                ) {
                    hang_reset!();
                    return true;
                }
            }
            let _ = play_block(queue, harness).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(landed, "the switch onto {id:?} must reach the player");
}

#[derive(Clone, Copy)]
enum InitialStart {
    Play,
    Select,
}

fn first_onset_frame(pcm: &[f32], threshold: f32) -> Option<usize> {
    let channels = usize::from(CHANNELS);
    pcm.chunks_exact(channels)
        .position(|frame| frame.iter().any(|s| s.abs() > threshold))
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayer,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(block_budget * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..block_budget {
        hang_tick!(format!("queue renders block after {} samples", pcm.len()));
        pcm.extend(play_block(queue, harness).await);
        hang_reset!();
    }
    pcm
}

/// Render `block_budget` blocks and return the second half, the part a level
/// verdict judges; the first half carries the switch's own start. The judged
/// half must play: every block sounds and the producer never starves, because
/// a zero-filled block would pass for the track falling silent.
async fn judged_half(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayer,
    block_budget: usize,
    track: &str,
) -> Vec<f32> {
    let _ = render_loop(queue, harness, block_budget / 2).await;
    let underruns = harness.metrics().underruns();
    let judged = render_loop(queue, harness, block_budget - block_budget / 2).await;
    for (block, samples) in judged
        .chunks(BLOCK_FRAMES * usize::from(CHANNELS))
        .enumerate()
    {
        let level = rms(samples);
        assert!(
            level > 0.0,
            "{track} must sound in judged block {block}: rms={level}"
        );
    }
    assert_eq!(
        harness.metrics().underruns(),
        underruns,
        "{track}: the judged window must not conceal producer starvation"
    );
    judged
}

/// Whether the player has already published an item's natural end.
fn played_to_end(events: &mut EventReceiver<TestEvent>) -> bool {
    loop {
        match events.try_recv() {
            Ok(envelope)
                if matches!(
                    envelope.event,
                    TestEvent::Player(PlayerEvent::ItemDidPlayToEnd { .. })
                ) =>
            {
                return true;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn stalled_successor(server: &TestServerHelper, label: &str) -> String {
    server
        .register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(assets::signal_mp3_saw_2s().bytes().to_vec()),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::StallAfter { after_bytes: 0 },
        })
        .child_url(label)
        .to_string()
}

#[kithara::test(tokio)]
async fn seek_updates_cached_position_optimistically() {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source = assets::constant_wav_quiet_120s();
    let id = append_loaded(&harness, &queue, &source).await;
    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select track");

    harness
        .run(&queue, |q| q.seek(54.689_879_542))
        .await
        .expect("seek must land");

    assert_eq!(queue.position_seconds(), Some(54.689_879_542));
    drop(queue);
    harness.close().await;
}

/// Track A reaches real EOF while the fixture server stalls B's HTTP load. Selecting
/// A again must reload the retained product source and restart playback.
#[kithara::test(tokio)]
async fn reselect_finished_track_restarts_when_next_track_never_loads() {
    const PASS_BLOCKS: usize = 64;

    let server = TestServerHelper::new().await;
    let (harness, queue) = offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .downloader(server.modeled_downloader())
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let source_a = assets::constant_wav_three_0_4s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let _id_b = harness
        .run(&queue, {
            let source = stalled_successor(&server, "stalled.mp3");
            move |q| q.append(source)
        })
        .await
        .expect("append a real stalled successor");

    let mut events = queue.subscribe::<TestEvent>();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    let first_pcm = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&first_pcm, 0.005).is_some(),
        "track A must play through on the first pass"
    );
    // Only A ever plays, so the player's end of an item is A's; a silent tail
    // would also be what a starved read leaves behind.
    assert!(
        played_to_end(&mut events),
        "track A must have played to its end before it is re-selected"
    );
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select of the finished track must be accepted");

    let second_pcm = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&second_pcm, 0.005).is_some(),
        "re-selecting track A after it played to EOF must restart playback"
    );
    assert_eq!(queue.current_index(), Some(0));
    drop(queue);
    harness.close().await;
}

/// Switching back to a consumed track must switch the audio, not merely the
/// selection bookkeeping. The judging window opens on the queue's own
/// statement that the switch reached the player, so what it measures is the
/// audio that follows it and not the reload it waited on.
///
/// Every verdict here is a track's level. The blocks play at a device's
/// cadence, so the decode has each block's period to fill it and a zero-filled
/// block is an underrun of the pipeline, not of the machine running the test;
/// each judged window therefore has to play without one.
#[kithara::test(tokio)]
#[case::selected(InitialStart::Select)]
#[case::play_button(InitialStart::Play)]
async fn switch_back_to_consumed_track_switches_audio(#[case] initial_start: InitialStart) {
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let source_a = assets::constant_wav_quiet_8s();
    let source_b = assets::constant_wav_loud_8s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let id_b = append_loaded(&harness, &queue, &source_b).await;

    match initial_start {
        InitialStart::Play => harness.run(&queue, QueueControl::play).await,
        InitialStart::Select => harness
            .run(&queue, move |q| q.select(id_a, Transition::None))
            .await
            .expect("select track A"),
    }
    let pcm_a = judged_half(&queue, &harness, WARMUP_BLOCKS, "track A").await;
    let mean_a = mean_abs(&pcm_a);
    assert!(mean_a > 0.005, "track A must start: mean={mean_a}");

    harness
        .run(&queue, move |q| q.select(id_b, Transition::None))
        .await
        .expect("select track B");
    let pcm_b = judged_half(&queue, &harness, WARMUP_BLOCKS, "track B").await;
    let mean_b = mean_abs(&pcm_b);
    assert!(
        mean_b > mean_a * 4.0,
        "track B must dominate after the switch: mean_a={mean_a}, mean_b={mean_b}"
    );

    let mut switches = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("switch back to track A");
    wait_for_current_track(&queue, &harness, &mut switches, id_a).await;
    let pcm = judged_half(&queue, &harness, WARMUP_BLOCKS, "switched-back track A").await;
    let mean_back = mean_abs(&pcm);
    // The selection an amplitude verdict is about: a switch-back that never
    // moved the queue and one that moved it while B kept sounding are
    // different defects, and the amplitudes alone do not tell them apart.
    let index_back = queue.current_index();
    assert!(
        mean_back > 0.005,
        "track A must be audible after the switch-back: mean={mean_back}, current_index={index_back:?}"
    );
    assert!(
        mean_back < mean_b / 4.0,
        "track B must stop sounding after the switch-back: mean_a={mean_a}, mean_b={mean_b}, \
         mean_back={mean_back}, current_index={index_back:?}"
    );
    assert_eq!(index_back, Some(0));
    drop(queue);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn play_button_marks_current_loaded_track_consumed() {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = assets::constant_wav_three_8s();
    let source_b = assets::constant_wav_three_8s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let _id_b = append_loaded(&harness, &queue, &source_b).await;

    harness.run(&queue, QueueControl::play).await;
    let _ = render_loop(&queue, &harness, 8).await;

    assert_eq!(
        queue.track(id_a).map(|entry| entry.status),
        Some(TrackStatus::Consumed),
        "play() consumed track A's slot resource; its status must say so"
    );
    drop(queue);
    harness.close().await;
}

/// Re-selecting A must cancel a pending switch to a real load that remains
/// unavailable, so its eventual completion cannot barge into playback.
#[kithara::test(tokio)]
async fn reselect_playing_track_cancels_pending_switch() {
    const WARMUP_BLOCKS: usize = 64;

    let server = TestServerHelper::new().await;
    let (harness, queue) = offline_queue_fixture_with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .downloader(server.modeled_downloader())
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let source_a = assets::constant_wav_three_5s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let id_b = harness
        .run(&queue, {
            let source = stalled_successor(&server, "pending.mp3");
            move |q| q.append(source)
        })
        .await
        .expect("append a real stalled successor");

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    let warmup_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    assert!(
        first_onset_frame(&warmup_pcm, 0.005).is_some(),
        "track A must be audible before the pending-switch step"
    );

    harness
        .run(&queue, move |q| q.select(id_b, Transition::None))
        .await
        .expect("select of a real still-loading track stashes a pending switch");
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select of the playing track");

    assert_eq!(
        queue.track(id_b).map(|entry| entry.status),
        Some(TrackStatus::Cancelled),
        "re-selecting the playing track must cancel the pending switch"
    );

    let after_pcm = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    assert!(
        first_onset_frame(&after_pcm, 0.005).is_some(),
        "track A must keep playing uninterrupted"
    );
    drop(queue);
    harness.close().await;
}

/// Repeat-one advances onto the very item that just ended, so the advance
/// runs while the render thread still reports the session as playing: it
/// clears that flag only at the top of the block after it queued the end.
/// The restart must not depend on the flag, and a prefetch reload is not a
/// substitute — it re-loads the resource without stashing a select, so
/// nothing would sound again.
///
/// The second pass waits for the queue's own statement that it answered the
/// end, not for a reload. Which of the three statuses the advance lands on
/// is a race the product does not control, and each reaches the restart by
/// its own route: in place, through a re-select of the reloaded entry, or
/// through a select the load applies. Only the advance is common to all
/// three, so waiting on a reload would assert whichever route won. Each pass
/// plays at a device's cadence, so it is a span of audio the restart has to
/// fill, not a race between the renders and the loader.
#[kithara::test(tokio, tracing("kithara_queue=debug,kithara_play=debug"))]
async fn repeat_one_restarts_the_track_its_own_eof_ended() {
    const PASS_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source = assets::constant_wav_three_0_4s();
    let id = append_loaded(&harness, &queue, &source).await;
    harness.run(&queue, |q| q.set_repeat(RepeatMode::One)).await;
    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select the only track");

    let mut advances = queue.subscribe();
    let first_pass = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&first_pass, 0.005).is_some(),
        "the track must play once before repeat-one is judged"
    );
    wait_for_eof_advance(&queue, &harness, &mut advances, id).await;

    let after_eof = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&after_eof, 0.005).is_some(),
        "repeat-one must restart the track its own EOF ended"
    );
    wait_for_eof_advance(&queue, &harness, &mut advances, id).await;
    let after_second_eof = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&after_second_eof, 0.005).is_some(),
        "repeat-one must restart after a second natural EOF"
    );
    drop(queue);
    harness.close().await;
}
