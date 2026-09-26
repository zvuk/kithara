#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    self,
    events::{EventReceiver, TrackId},
    platform::{
        sync::Arc,
        time::{self, Duration},
    },
    queue::{AdvanceReason, QueueControl, QueueEvent, RepeatMode, TrackStatus, Transition},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper,
    offline::{OfflinePlayer, append_loaded, offline_queue_fixture},
};
use kithara_test_fixtures::{assets, signal::mean_abs};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const BLOCK_BUDGET: usize = 256;

/// The advance a natural end asks for, named for the entry it lands on.
async fn wait_for_eof_advance(events: &mut EventReceiver<QueueEvent>, id: TrackId) {
    let answered = time::timeout(Duration::from_secs(20), async {
        while let Ok(envelope) = events.recv().await {
            if matches!(
                envelope.event,
                QueueEvent::CurrentTrackAdvance {
                    id: Some(seen),
                    reason: AdvanceReason::NaturalEof,
                } if seen == id
            ) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        answered,
        "repeat-one must answer the end of {id:?} with an advance onto it"
    );
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

async fn render_loop(
    queue: &QueueControl<TestPools>,
    harness: &OfflinePlayer,
    block_budget: usize,
) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(block_budget * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..block_budget {
        let _ = harness.run(queue, |q| q.tick()).await;
        pcm.extend(harness.render(BLOCK_FRAMES).await);
    }
    pcm
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

    queue.seek(54.689_879_542).expect("seek must land");

    assert_eq!(queue.position_seconds(), Some(54.689_879_542));
    drop(queue);
    harness.close().await;
}

/// Track A reaches real EOF while B is a real HTTP load that stalls. Selecting
/// A again must reload the retained product source and restart playback.
#[kithara::test(tokio, flash(false))]
async fn reselect_finished_track_restarts_when_next_track_never_loads() {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = assets::constant_wav_three_0_4s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let server = TestServerHelper::new().await;
    let _id_b = harness
        .run(&queue, {
            let source = stalled_successor(&server, "stalled.mp3");
            move |q| q.append(source)
        })
        .await
        .expect("append a real stalled successor");

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("select track A");
    let first_pcm = render_loop(&queue, &harness, BLOCK_BUDGET).await;
    assert!(
        first_onset_frame(&first_pcm, 0.005).is_some(),
        "track A must play through on the first pass"
    );
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select of the finished track must be accepted");

    let second_pcm = render_loop(&queue, &harness, BLOCK_BUDGET).await;
    assert!(
        first_onset_frame(&second_pcm, 0.005).is_some(),
        "re-selecting track A after it played to EOF must restart playback"
    );
    assert_eq!(queue.current_index(), Some(0));
    drop(queue);
    harness.close().await;
}

/// Switching back to a consumed track must switch the audio, not merely the
/// selection bookkeeping.
#[kithara::test(tokio, flash(false))]
#[case::selected(InitialStart::Select)]
#[case::play_button(InitialStart::Play)]
async fn switch_back_to_consumed_track_switches_audio(#[case] initial_start: InitialStart) {
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = assets::constant_wav_quiet_8s();
    let source_b = assets::constant_wav_loud_8s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let id_b = append_loaded(&harness, &queue, &source_b).await;

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

    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("switch back to track A");
    let pcm = render_loop(&queue, &harness, WARMUP_BLOCKS).await;
    let mean_back = mean_abs(&pcm[pcm.len() / 2..]);
    assert!(
        mean_back > 0.005,
        "track A must be audible after the switch-back: mean={mean_back}"
    );
    assert!(
        mean_back < mean_b / 4.0,
        "track B must stop sounding after the switch-back: mean_b={mean_b}, mean_back={mean_back}"
    );
    assert_eq!(queue.current_index(), Some(0));
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

    harness.run(&queue, move |q| q.play()).await;
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

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = assets::constant_wav_three_5s();
    let id_a = append_loaded(&harness, &queue, &source_a).await;
    let server = TestServerHelper::new().await;
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
/// three, so waiting on a reload would assert whichever route won, and a
/// fixed budget of immediate renders would assert the loader's latency.
#[kithara::test(tokio, flash(false))]
async fn repeat_one_restarts_the_track_its_own_eof_ended() {
    const PASS_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source = assets::constant_wav_three_0_4s();
    let id = append_loaded(&harness, &queue, &source).await;
    queue.set_repeat(RepeatMode::One);
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
    wait_for_eof_advance(&mut advances, id).await;

    let after_eof = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&after_eof, 0.005).is_some(),
        "repeat-one must restart the track its own EOF ended"
    );
    wait_for_eof_advance(&mut advances, id).await;
    let after_second_eof = render_loop(&queue, &harness, PASS_BLOCKS).await;
    assert!(
        first_onset_frame(&after_second_eof, 0.005).is_some(),
        "repeat-one must restart after a second natural EOF"
    );
    drop(queue);
    harness.close().await;
}
