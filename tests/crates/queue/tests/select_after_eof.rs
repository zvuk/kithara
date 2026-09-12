#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    self,
    platform::sync::Arc,
    queue::{QueueControl, TrackStatus, Transition},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper,
    offline::{OfflinePlayerHarness, mean_abs, offline_queue_fixture},
};
use kithara_test_fixtures::{
    assets,
    integration_fixtures::{constant_loud, constant_quiet, constant_three},
};

use crate::{
    bufpool_ext::TestPools,
    loader_fixture::{LocalWav, append_loaded, wait_loaded},
};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const BLOCK_BUDGET: usize = 256;

#[derive(Clone, Copy)]
enum InitialStart {
    Play,
    Select,
}

fn local_wav(label: &str, secs: f64, samples: &'static [u8]) -> LocalWav {
    LocalWav::constant(label, SAMPLE_RATE, CHANNELS, secs, samples)
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
async fn seek_updates_cached_position_optimistically(constant_quiet: &'static [u8]) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source = local_wav("seek", 120.0, constant_quiet);
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
async fn reselect_finished_track_restarts_when_next_track_never_loads(
    constant_three: &'static [u8],
) {
    const TRACK_SECS: f64 = 0.4;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = local_wav("reselect-eof", TRACK_SECS, constant_three);
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
    let mut reload_events = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("re-select of the finished track must be accepted");
    wait_loaded(&mut reload_events, id_a).await;

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
async fn switch_back_to_consumed_track_switches_audio(
    #[case] initial_start: InitialStart,
    constant_loud: &'static [u8],
    constant_quiet: &'static [u8],
) {
    const TRACK_SECS: f64 = 8.0;
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = local_wav("switch-back-a", TRACK_SECS, constant_quiet);
    let source_b = local_wav("switch-back-b", TRACK_SECS, constant_loud);
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

    let mut reload_events = queue.subscribe();
    harness
        .run(&queue, move |q| q.select(id_a, Transition::None))
        .await
        .expect("switch back to track A");
    wait_loaded(&mut reload_events, id_a).await;
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
async fn play_button_marks_current_loaded_track_consumed(constant_three: &'static [u8]) {
    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = local_wav("play-button-a", 8.0, constant_three);
    let source_b = local_wav("play-button-b", 8.0, constant_three);
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
async fn reselect_playing_track_cancels_pending_switch(constant_three: &'static [u8]) {
    const TRACK_SECS: f64 = 5.0;
    const WARMUP_BLOCKS: usize = 64;

    let (harness, queue) = offline_queue_fixture(SAMPLE_RATE).await;
    let source_a = local_wav("cancel-pending", TRACK_SECS, constant_three);
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
