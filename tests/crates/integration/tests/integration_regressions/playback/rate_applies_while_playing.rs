#![cfg(not(target_arch = "wasm32"))]

use std::future::Future;

use kithara::{
    events::TrackId,
    platform::time::{self, Duration, WallInstant},
    play::{PlayerEvent, TrackSettingsChange},
    signal::AudioChunkInfo,
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions};
use kithara_test_fixtures::assets;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const WARMUP_BLOCKS: usize = 8;
const CLOCK_BLOCKS: usize = 32;
/// Wide enough for the rate-1.0 baseline to reach silence inside the window.
/// A window that clips the baseline makes both sides saturate at the cap and
/// the comparison below vacuous.
const MEASURE_BLOCKS: usize = 200;
const FAST_RATE: f32 = 2.0;

#[kithara::test(tokio)]
#[kithara_test_utils::kithara::hang_watchdog]
async fn fixed_rate_reader_keeps_source_and_player_clock_at_unity() {
    let oracle = loaded_harness(1.0).await;
    assert_eq!(oracle.player().rate(), 1.0);
    completed(oracle.with_queue(kithara::queue::QueueControl::pause)).await;
    assert_eq!(
        oracle.player().rate(),
        1.0,
        "the control thread must not publish pause before RT applies it"
    );
    let _ = completed(oracle.render(BLOCK_FRAMES)).await;
    let paused_rates = rate_events(completed(oracle.tick_and_drain()).await);
    assert_eq!(paused_rates, [0.0]);
    assert_eq!(oracle.player().rate(), 0.0);

    completed(
        oracle.with_queue(move |player| {
            player.configure_track(TrackSettingsChange::Speed(FAST_RATE))
        }),
    )
    .await
    .expect("a finite rate is accepted");
    wait_for_default_rate(&oracle).await;
    assert_eq!(oracle.player().default_rate(), FAST_RATE);
    hang_reset!();
    completed(oracle.with_queue(kithara::queue::QueueControl::play)).await;
    assert_eq!(
        oracle.player().rate(),
        0.0,
        "the control thread must not publish the requested rate before RT applies it"
    );
    let _ = completed(oracle.render(BLOCK_FRAMES)).await;
    let resumed_rates = rate_events(completed(oracle.tick_and_drain()).await);
    assert_eq!(resumed_rates, [FAST_RATE]);
    assert_eq!(oracle.player().rate(), FAST_RATE);

    let baseline = blocks_until_silence(1.0).await;
    let requested_fast = blocks_until_silence(FAST_RATE).await;
    let baseline_advance = media_advance(1.0).await;
    let requested_fast_advance = media_advance(FAST_RATE).await;

    assert!(
        baseline < MEASURE_BLOCKS,
        "the rate-1.0 baseline must drain inside the measured window, \
         got {baseline} of {MEASURE_BLOCKS} blocks — the comparison below would \
         compare two saturated caps"
    );
    assert_eq!(
        requested_fast + WARMUP_BLOCKS - 1,
        (baseline + WARMUP_BLOCKS - 1).div_ceil(2),
        "a rate-2 lane must exhaust the same PCM in half the output blocks, \
         including warmup and excluding the first silent block"
    );
    assert!(
        (requested_fast_advance - baseline_advance * f64::from(FAST_RATE)).abs() < f64::EPSILON,
        "the lane's media clock must follow its PCM speed exactly: \
         {requested_fast_advance}s vs {baseline_advance}s at {FAST_RATE}"
    );
    completed(oracle.close()).await;
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn completed<Output>(operation: impl Future<Output = Output>) -> Output {
    let mut operation = std::pin::pin!(operation);
    loop {
        hang_tick!();
        if let Ok(output) = time::timeout(Duration::from_millis(10), operation.as_mut()).await {
            hang_reset!();
            return output;
        }
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn wait_for_default_rate(oracle: &OfflinePlayer) {
    let deadline = WallInstant::now() + Duration::from_secs(5);
    while oracle.player().default_rate() != FAST_RATE && WallInstant::now() < deadline {
        hang_tick!();
        let _ = completed(oracle.render(BLOCK_FRAMES)).await;
        completed(oracle.tick_and_drain()).await;
        time::sleep(Duration::from_millis(5)).await;
    }
    if oracle.player().default_rate() == FAST_RATE {
        hang_reset!();
    }
}

fn rate_events(events: Vec<PlayerEvent>) -> Vec<f32> {
    events
        .into_iter()
        .filter_map(|event| match event {
            PlayerEvent::RateChanged { rate } => Some(rate),
            PlayerEvent::StatusChanged { .. }
            | PlayerEvent::TimeControlStatusChanged { .. }
            | PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::VolumeChanged { .. }
            | PlayerEvent::MuteChanged { .. }
            | PlayerEvent::CurrentItemChanged { .. }
            | PlayerEvent::PrerollCompleted { .. }
            | PlayerEvent::ItemDidPlayToEnd { .. }
            | PlayerEvent::ItemDidFail { .. } => None,
        })
        .collect()
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn loaded_harness(rate: f32) -> OfflinePlayer {
    let harness = completed(OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .build(),
        SAMPLE_RATE,
    ))
    .await;
    let deck_source = assets::constant_half_wav_default()
        .path()
        .expect("prepared float WAV has a store path")
        .to_str()
        .expect("UTF-8 fixture path")
        .to_owned();
    let deck_id = completed(harness.with_queue(move |player| {
        player
            .configure_track(TrackSettingsChange::Speed(rate))
            .expect("initial lane speed");
        let deck_id = TrackId::allocate();
        player
            .append_with_id(deck_id, deck_source)
            .expect("append PCM deck");
        player
            .select(deck_id, kithara::queue::Transition::None)
            .expect("select the item");
        deck_id
    }))
    .await;

    completed(harness.render_until_current(
        deck_id,
        BLOCK_FRAMES,
        WallInstant::now() + Duration::from_secs(5),
    ))
    .await;
    hang_reset!();
    completed(harness.with_queue(kithara::queue::QueueControl::play)).await;
    for _ in 0..WARMUP_BLOCKS {
        hang_tick!();
        let _ = completed(harness.render(BLOCK_FRAMES)).await;
        let _ = completed(harness.tick_and_drain()).await;
        hang_reset!();
    }
    harness
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn blocks_until_silence(rate: f32) -> usize {
    let harness = loaded_harness(rate).await;

    let mut blocks = 0usize;
    for _ in 0..MEASURE_BLOCKS {
        hang_tick!();
        let block = completed(harness.render(BLOCK_FRAMES)).await;
        let _ = completed(harness.tick_and_drain()).await;
        blocks = blocks.saturating_add(1);
        hang_reset!();
        if block.iter().all(|sample| sample.abs() == 0.0) {
            break;
        }
    }
    completed(harness.close()).await;
    blocks
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn media_advance(rate: f32) -> f64 {
    let harness = loaded_harness(rate).await;
    let (start, source) = source_frame(&harness);
    for _ in 0..CLOCK_BLOCKS {
        hang_tick!();
        let _ = completed(harness.render(BLOCK_FRAMES)).await;
        let _ = completed(harness.tick_and_drain()).await;
        hang_reset!();
    }
    let (end, finished) = source_frame(&harness);
    assert_eq!(
        (
            source.segment,
            source.source_revision,
            source.render_revision,
            source.mapping_revision
        ),
        (
            finished.segment,
            finished.source_revision,
            finished.render_revision,
            finished.mapping_revision
        ),
        "the fixed source keeps its identity during the measured blocks",
    );
    let advance = u32::try_from(end.checked_sub(start).expect("source clock advances"))
        .expect("bounded measured source frames");
    let output_frames = u32::try_from(CLOCK_BLOCKS * BLOCK_FRAMES).expect("bounded output frames");
    assert_eq!(
        f64::from(advance),
        f64::from(output_frames) * f64::from(rate),
        "every measured output frame advances the exact requested source clock",
    );
    completed(harness.close()).await;
    f64::from(advance) / f64::from(SAMPLE_RATE)
}

fn source_frame(harness: &OfflinePlayer) -> (u64, AudioChunkInfo) {
    let snapshot = harness.deck_snapshot();
    let mut playing = snapshot
        .slots
        .iter()
        .filter(|slot| slot.state == kithara_render::bridge::SlotState::Playing);
    let slot = playing.next().expect("the fixed reader is playing");
    assert!(playing.next().is_none(), "exactly one fixed reader plays");
    let mark = slot.mark.expect("applied source mark");
    let source = slot.source.expect("consumed source identity");
    assert_eq!(mark.lane.segment, source.segment);
    assert_eq!(source.media_origin, Some(Duration::ZERO));
    assert_eq!(source.spec.sample_rate.get(), SAMPLE_RATE);
    let offset = mark
        .lane
        .frame
        .checked_sub(source.lane_frame)
        .expect("mark follows source start");
    let span = source
        .source_geometry()
        .expect("exact consumed source clock");
    assert_eq!(span.render_revision(), source.render_revision);
    assert_eq!(span.mapping_revision(), source.mapping_revision);
    assert!(offset <= span.output_frames());
    let (numerator, denominator) = span
        .source_ratio_at(offset)
        .expect("source coordinate at mark");
    assert_eq!(
        numerator % denominator.get(),
        0,
        "integer-speed source frame"
    );
    let frame = u64::try_from(numerator / denominator.get()).expect("source frame fits u64");
    assert_eq!(
        harness
            .player()
            .position_seconds()
            .expect("published player position"),
        source
            .spec
            .duration_for(frame)
            .expect("published source time")
            .as_secs_f64(),
        "the public clock agrees exactly at its nanosecond precision",
    );
    (frame, source)
}
