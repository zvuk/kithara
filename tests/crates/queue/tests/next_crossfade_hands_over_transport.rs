#![cfg(not(target_arch = "wasm32"))]

//! A Next press crossfades into the following track and hands the deck's
//! transport to it: once the fade lands, the queue names the incoming track
//! as current, and Pause and Play reach the track that now sounds rather than
//! the one the fade silenced.

use kithara::{
    events::{EventReceiver, TrackId},
    platform::time::{self, Duration, sleep},
    queue::{QueueControl, QueueEvent, Transition},
};
use kithara_integration_tests::{
    kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions, asset_source},
};
use kithara_test_fixtures::{assets, signal::rms};
use num_traits::AsPrimitive;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// Blocks in one second of output at [`SAMPLE_RATE`], rounded up.
const BLOCKS_PER_SECOND: usize = (SAMPLE_RATE as usize).div_ceil(BLOCK_FRAMES);

/// Renders `blocks` output blocks, giving the queue an owner pass before each
/// one the way the app's engine tick does, and returns the last block.
async fn render(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    blocks: usize,
) -> Vec<f32> {
    let mut last = Vec::new();
    for _ in 0..blocks {
        harness
            .run(queue, QueueControl::tick)
            .await
            .expect("the queue takes its owner pass");
        last = harness.render(BLOCK_FRAMES).await;
    }
    last
}

/// Renders one block at a time until `done` holds, failing after `within`.
async fn render_until(
    harness: &OfflinePlayer,
    queue: &QueueControl<TestPools>,
    what: &str,
    within: Duration,
    mut done: impl FnMut(&QueueControl<TestPools>) -> bool,
) {
    let deadline = time::Instant::now() + within;
    loop {
        render(harness, queue, 1).await;
        if done(queue) {
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

fn current_changed_to(events: &mut EventReceiver<QueueEvent>, id: TrackId) -> bool {
    let mut seen = false;
    while let Ok(event) = events.try_recv() {
        seen |= matches!(event.event, QueueEvent::CurrentTrackChanged { id: Some(current) } if current == id);
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
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(crossfade_seconds)
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    )
    .await;
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
    )
    .await;
    assert_eq!(queue.current().map(|track| track.id), Some(outgoing));

    let mut events: EventReceiver<QueueEvent> = queue.subscribe();
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
    )
    .await;
    assert_eq!(queue.current().map(|track| track.id), Some(incoming));
    let fade_seconds: usize = crossfade_seconds.ceil().as_();
    render(&harness, &queue, (fade_seconds + 1) * BLOCKS_PER_SECOND).await;

    harness.run(&queue, QueueControl::pause).await;
    render(&harness, &queue, BLOCKS_PER_SECOND / 2).await;
    assert!(
        !queue.is_playing(),
        "Pause reaches the track that now sounds"
    );
    let held = queue
        .position_seconds()
        .expect("the paused track keeps a position");
    let paused = render(&harness, &queue, BLOCKS_PER_SECOND).await;
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

    harness.run(&queue, QueueControl::play).await;
    let resumed = render(&harness, &queue, BLOCKS_PER_SECOND).await;
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

    drop(events);
    drop(queue);
    harness.close().await;
}

/// A track at a set tempo keeps sounding while an offline session renders it:
/// the render waits for the stretched lane rather than outrunning it, so no
/// window of the output is silence and the playhead moves at the tempo.
#[kithara::test(tokio)]
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
    )
    .await;

    let window_blocks = BLOCKS_PER_SECOND / 4;
    let window_frames: f64 = (window_blocks * BLOCK_FRAMES).as_();
    let window_seconds = window_frames / f64::from(SAMPLE_RATE);
    let start = queue
        .position_seconds()
        .expect("the playing track has a position");
    for window in 0..WINDOWS {
        let pcm = render(&harness, &queue, window_blocks).await;
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
