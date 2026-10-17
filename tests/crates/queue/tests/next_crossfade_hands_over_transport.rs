#![cfg(not(target_arch = "wasm32"))]

//! A Next press crossfades into the following track and hands the deck's
//! transport to it: once the fade lands, the queue names the incoming track
//! as current, and Pause and Play reach the track that now sounds rather than
//! the one the fade silenced.

use std::num::NonZeroU32;

use kithara::{
    events::{EventReceiver, TrackId},
    host::{HostConfig, HostSettings, MetronomeConfig, Tap},
    platform::time::{self, Duration, sleep},
    queue::{QueueControl, QueueEvent, Transition},
};
use kithara_integration_tests::{
    audio_artifact::{AudioArtifactTap, artifact_label},
    kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions, TapProbe, asset_source},
    usdt_trace,
};
use kithara_test_fixtures::{assets, signal::rms};
use num_traits::AsPrimitive;

use crate::bufpool_ext::{TestPools, pools};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// Blocks in one second of output at [`SAMPLE_RATE`], rounded up.
const BLOCKS_PER_SECOND: usize = (SAMPLE_RATE as usize).div_ceil(BLOCK_FRAMES);

/// Renders `blocks` output blocks, giving the queue an owner pass before each
/// one the way the app's engine tick does, and returns the last block.
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn render(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    blocks: usize,
    mut capture: Option<&mut (AudioArtifactTap, TapProbe)>,
) -> Vec<f32> {
    let mut last = Vec::new();
    for block_index in 0..blocks {
        hang_tick!(format!("rendering transport block {block_index}/{blocks}"));
        harness
            .run(queue, QueueControl::tick)
            .await
            .expect("the queue takes its owner pass");
        last = harness.render(BLOCK_FRAMES).await;
        if let Some((artifact, master)) = capture.as_deref_mut() {
            artifact.push(&last);
            last = master.drain();
            assert_eq!(master.drops(), 0, "the master tap keeps every frame");
            assert_eq!(
                last.len(),
                BLOCK_FRAMES * 2,
                "the master tap sees the complete block"
            );
        }
        hang_reset!();
    }
    last
}

/// Renders one block at a time until `done` holds, failing after `within`.
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
#[kithara_test_utils::kithara::flash(io)]
async fn render_until(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    what: &str,
    within: Duration,
    mut done: impl FnMut(&QueueControl<TestPools>) -> bool,
    mut capture: Option<&mut (AudioArtifactTap, TapProbe)>,
) {
    let deadline = time::Instant::now() + within;
    loop {
        hang_tick!(format!(
            "{what}: current={:?}",
            queue.current().map(|track| track.id)
        ));
        render(harness, queue, 1, capture.as_deref_mut()).await;
        if done(queue) {
            hang_reset!();
            return;
        }
        assert!(
            time::Instant::now() < deadline,
            "{what}: not within {within:?}: current={:?}, playing={}, position={:?}",
            queue.current().map(|track| (track.id, track.status)),
            queue.is_playing(),
            queue.position_seconds()
        );
        sleep(Duration::from_millis(1)).await;
    }
}

#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
fn current_changed_to(events: &mut EventReceiver<QueueEvent>, id: TrackId) -> bool {
    let mut seen = false;
    while let Ok(event) = events.try_recv() {
        hang_tick!(format!("draining changes while waiting for track {id:?}"));
        seen |= matches!(event.event, QueueEvent::CurrentTrackChanged { id: Some(current) } if current == id);
        hang_reset!();
    }
    seen
}

#[kithara::test(tokio)]
#[case::one_second_fade(1.0, None)]
#[case::the_apps_five_second_fade(5.0, None)]
#[case::five_second_fade_at_a_set_tempo(5.0, Some(1.06))]
async fn a_next_press_hands_the_transport_to_the_track_it_crossfades_in(
    #[case] crossfade_seconds: f32,
    #[case] tempo: Option<f32>,
) {
    let artifact = AudioArtifactTap::from_env(&artifact_label(), SAMPLE_RATE, 2)
        .expect("crossfade listening artifact");
    let _trace = artifact.as_ref().map(|_| usdt_trace::scope());
    let session = HostConfig::offline(pools())
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("sample rate"))
                .metronome(
                    MetronomeConfig::builder()
                        .enabled(artifact.is_some())
                        .build(),
                )
                .build(),
        )
        .build();
    let harness = OfflinePlayer::with_options(
        OfflinePlayerOptions::builder()
            .crossfade_duration(crossfade_seconds)
            .block_on_underrun(true)
            .build(),
        session,
    )
    .await;
    let mut capture = match artifact {
        Some(mut artifact) => {
            artifact.evidence(
                "capture_origin_session_frame",
                serde_json::json!(harness.host().position()),
            );
            artifact.evidence(
                "signal",
                serde_json::json!("constant-level DC transport diagnostic"),
            );
            artifact.evidence("crossfade_seconds", serde_json::json!(crossfade_seconds));
            artifact.evidence("requested_playing_rate", serde_json::json!(tempo));
            artifact.evidence("listening_tap", serde_json::json!("Host Output"));
            artifact.evidence("assertion_tap", serde_json::json!("Host Master"));
            let master = harness
                .host()
                .attach_tap(Tap::Master, BLOCK_FRAMES * 2)
                .await
                .expect("capture the same pre-metronome PCM as the ordinary scenario");
            Some((artifact, master))
        }
        None => None,
    };
    let queue = harness.player().clone();
    let quiet = asset_source(&assets::constant_wav_quiet_30s());
    let loud = asset_source(&assets::constant_wav_loud_30s());
    let outgoing = harness
        .run(&queue, move |q| q.append(quiet))
        .await
        .expect("the deck takes the first track");
    let incoming = harness
        .run(&queue, move |q| q.append(loud))
        .await
        .expect("the deck takes the next track");
    harness
        .run(&queue, move |q| q.select(outgoing, Transition::None))
        .await
        .expect("the first track is selected");
    if let Some(rate) = tempo {
        harness
            .run(&queue, move |q| q.set_rate(rate))
            .await
            .expect("the deck takes the tempo");
    }
    harness.run(&queue, QueueControl::play).await;
    render_until(
        &harness,
        &queue,
        "the first track plays",
        Duration::from_secs(10),
        |q| q.is_playing() && q.position_seconds().is_some_and(|seconds| seconds > 1.0),
        capture.as_mut(),
    )
    .await;
    assert_eq!(queue.current().map(|track| track.id), Some(outgoing));

    let mut events: EventReceiver<QueueEvent> = queue.subscribe();
    if let Some((artifact, _)) = capture.as_mut() {
        artifact.mark("Next requested");
    }
    harness
        .run(&queue, move |q| q.next(Transition::Crossfade))
        .await
        .expect("the Next press is accepted");
    let mut handed_over = false;
    render_until(
        &harness,
        &queue,
        "the crossfade hands the deck to the incoming track",
        Duration::from_secs_f32(crossfade_seconds + 10.0),
        |_| {
            handed_over |= current_changed_to(&mut events, incoming);
            handed_over
        },
        capture.as_mut(),
    )
    .await;
    assert_eq!(queue.current().map(|track| track.id), Some(incoming));
    if let Some((artifact, _)) = capture.as_mut() {
        artifact.mark("CurrentTrackChanged observed for incoming track");
        artifact.evidence(
            "handover_observed_session_frame",
            serde_json::json!(harness.host().position()),
        );
        artifact.evidence("incoming_track", serde_json::json!(u64::from(incoming)));
    }
    let fade_seconds: usize = crossfade_seconds.ceil().as_();
    render(
        &harness,
        &queue,
        (fade_seconds + 1) * BLOCKS_PER_SECOND,
        capture.as_mut(),
    )
    .await;

    if let Some((artifact, _)) = capture.as_mut() {
        artifact.mark("Pause requested");
    }
    harness.run(&queue, QueueControl::pause).await;
    render(&harness, &queue, BLOCKS_PER_SECOND / 2, capture.as_mut()).await;
    assert!(
        !queue.is_playing(),
        "Pause reaches the track that now sounds"
    );
    let held = queue
        .position_seconds()
        .expect("the paused track keeps a position");
    let paused = render(&harness, &queue, BLOCKS_PER_SECOND, capture.as_mut()).await;
    let after = queue
        .position_seconds()
        .expect("the paused track keeps a position");
    assert!(
        (after - held).abs() < 0.05,
        "the paused track holds its position: {held:.3} -> {after:.3}"
    );
    assert!(
        rms(&paused) < 1e-4,
        "the paused deck is silent: rms {}",
        rms(&paused)
    );

    if let Some((artifact, _)) = capture.as_mut() {
        artifact.mark("Play requested after pause");
    }
    harness.run(&queue, QueueControl::play).await;
    let resumed = render(&harness, &queue, BLOCKS_PER_SECOND, capture.as_mut()).await;
    assert!(queue.is_playing(), "Play reaches the track that now sounds");
    assert_eq!(queue.current().map(|track| track.id), Some(incoming));
    let moved = queue
        .position_seconds()
        .expect("the resumed track keeps a position");
    assert!(
        moved > after + 0.5,
        "the resumed track advances: {after:.3} -> {moved:.3}"
    );
    assert!(
        rms(&resumed) > 1e-3,
        "the resumed track sounds: rms {}",
        rms(&resumed)
    );

    if let Some((artifact, _)) = capture.as_mut() {
        artifact.mark("incoming track resumed; all transport assertions passed");
    }
    drop(capture);
    drop(events);
    drop(queue);
    harness.close().await;
}

/// A track at a set tempo keeps sounding while an offline session renders it:
/// the render waits for the stretched lane rather than outrunning it, so no
/// window of the output is silence and the playhead moves at the tempo.
#[kithara::test(tokio)]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn a_track_at_a_set_tempo_keeps_sounding_through_an_offline_render() {
    const RATE: f32 = 1.06;
    const WINDOWS: usize = 12;
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness.player().clone();
    let source = asset_source(&assets::constant_wav_quiet_30s());
    let track = harness
        .run(&queue, move |q| q.append(source))
        .await
        .expect("the deck takes the track");
    harness
        .run(&queue, move |q| q.select(track, Transition::None))
        .await
        .expect("the track is selected");
    harness
        .run(&queue, move |q| q.set_rate(RATE))
        .await
        .expect("the deck takes the tempo");
    harness.run(&queue, QueueControl::play).await;
    render_until(
        &harness,
        &queue,
        "the track plays",
        Duration::from_secs(10),
        |q| q.is_playing() && q.position_seconds().is_some_and(|seconds| seconds > 0.0),
        None,
    )
    .await;

    let window_blocks = BLOCKS_PER_SECOND / 4;
    let window_frames: f64 = (window_blocks * BLOCK_FRAMES).as_();
    let window_seconds = window_frames / f64::from(SAMPLE_RATE);
    let start = queue
        .position_seconds()
        .expect("the playing track has a position");
    for window in 0..WINDOWS {
        hang_tick!(format!("tempo render window {window}"));
        let pcm = render(&harness, &queue, window_blocks, None).await;
        hang_reset!();
        assert!(
            rms(&pcm) > 0.05,
            "window {window} sounds: rms {}",
            rms(&pcm)
        );
    }
    let moved = queue
        .position_seconds()
        .expect("the playing track has a position")
        - start;
    let windows: f64 = WINDOWS.as_();
    let expected = window_seconds * windows * f64::from(RATE);
    assert!(
        (moved - expected).abs() < 0.1,
        "the playhead moves at the tempo: {moved:.3}s against {expected:.3}s"
    );
    drop(queue);
    harness.close().await;
}
