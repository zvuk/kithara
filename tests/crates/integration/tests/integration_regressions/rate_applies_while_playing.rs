#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    events::{PlayerEvent, TrackId},
    play::Resource,
    signal::AudioSpec,
};
use kithara_integration_tests::offline::{
    OfflinePlayerHarness, OfflinePlayerOptions, resource_from_reader,
};
use kithara_test_fixtures::integration_fixtures::constant_half;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const WARMUP_BLOCKS: usize = 8;
const CLOCK_BLOCKS: usize = 32;
/// Wide enough for the rate-1.0 baseline to reach silence inside the window.
/// A window that clips the baseline makes both sides saturate at the cap and
/// the comparison below vacuous.
const MEASURE_BLOCKS: usize = 200;
const FAST_RATE: f32 = 2.0;

fn make_resource(constant_half: &'static [u8], duration_secs: f64) -> Resource {
    resource_from_reader(
        kithara_integration_tests::audio_mock::TestPcmReader::from_pcm(
            AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            duration_secs,
            constant_half,
        ),
    )
}

#[kithara::test(tokio)]
async fn fixed_rate_reader_keeps_source_and_player_clock_at_unity(constant_half: &'static [u8]) {
    let oracle = loaded_harness(constant_half).await;
    assert_eq!(oracle.player().rate(), 1.0);
    oracle.player().pause();
    assert_eq!(
        oracle.player().rate(),
        1.0,
        "the control thread must not publish pause before RT applies it"
    );
    let _ = oracle.render(BLOCK_FRAMES).await;
    let paused_rates = rate_events(oracle.tick_and_drain().await);
    assert_eq!(paused_rates, [0.0]);
    assert_eq!(oracle.player().rate(), 0.0);

    oracle.player().set_default_rate(FAST_RATE);
    assert_eq!(oracle.player().default_rate(), FAST_RATE);
    oracle.player().play();
    assert_eq!(
        oracle.player().rate(),
        0.0,
        "the control thread must not publish the requested rate before RT applies it"
    );
    let _ = oracle.render(BLOCK_FRAMES).await;
    let resumed_rates = rate_events(oracle.tick_and_drain().await);
    assert_eq!(resumed_rates, [1.0]);
    assert_eq!(oracle.player().rate(), 1.0);

    let baseline = blocks_until_silence(constant_half, 1.0).await;
    let requested_fast = blocks_until_silence(constant_half, FAST_RATE).await;
    let baseline_advance = media_advance(constant_half, 1.0).await;
    let requested_fast_advance = media_advance(constant_half, FAST_RATE).await;

    assert!(
        baseline < MEASURE_BLOCKS,
        "the rate-1.0 baseline must drain inside the measured window, \
         got {baseline} of {MEASURE_BLOCKS} blocks — the comparison below would \
         compare two saturated caps"
    );
    assert_eq!(
        requested_fast, baseline,
        "a reader without a Warp control must stay fixed-rate instead of \
         consuming source frames at the requested rate"
    );
    assert!(
        (requested_fast_advance - baseline_advance).abs() < f64::EPSILON,
        "a reader without a Warp control must not report a media clock that \
        its PCM cannot follow: {requested_fast_advance}s vs {baseline_advance}s"
    );
    oracle.close().await;
}

fn rate_events(events: Vec<PlayerEvent>) -> Vec<f32> {
    events
        .into_iter()
        .filter_map(|event| match event {
            PlayerEvent::RateChanged { rate } => Some(rate),
            _ => None,
        })
        .collect()
}

async fn loaded_harness(constant_half: &'static [u8]) -> OfflinePlayerHarness {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder().build(),
        SAMPLE_RATE,
    )
    .await;
    harness
        .with_player(move |player| {
            player.insert(make_resource(constant_half, 1.0), TrackId::allocate(), None);
            player
                .select_item(0, true)
                .expect("select first queue item");
        })
        .await;

    for _ in 0..WARMUP_BLOCKS {
        let _ = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
    }
    harness
}

async fn blocks_until_silence(constant_half: &'static [u8], rate: f32) -> usize {
    let harness = loaded_harness(constant_half).await;
    harness.player().set_default_rate(rate);

    let mut blocks = 0usize;
    for _ in 0..MEASURE_BLOCKS {
        let block = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
        blocks = blocks.saturating_add(1);
        if block.iter().all(|sample| sample.abs() == 0.0) {
            break;
        }
    }
    harness.close().await;
    blocks
}

async fn media_advance(constant_half: &'static [u8], rate: f32) -> f64 {
    let harness = loaded_harness(constant_half).await;
    let start = harness.player().position_seconds().unwrap_or(0.0);
    harness.player().set_default_rate(rate);
    for _ in 0..CLOCK_BLOCKS {
        let _ = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
    }
    let advance = harness.player().position_seconds().unwrap_or(0.0) - start;
    harness.close().await;
    advance
}
