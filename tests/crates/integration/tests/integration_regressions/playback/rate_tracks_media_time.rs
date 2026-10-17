#![cfg(not(target_arch = "wasm32"))]

use std::{future::Future, path::Path};

use kithara::{
    assets::{AssetStore, StorageBackend},
    platform::time::{self, Duration},
    play::{PlayerEvent, ResourceConfig, ResourceSrc},
};
use kithara_integration_tests::{
    bufpool_ext::TestPools,
    kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions},
};
use kithara_test_fixtures::{fixtures::tone_mp3, integration_fixtures::drain_tone};
use kithara_test_utils::{TestTempDir, temp_dir};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// Long enough for the ring to have flushed every block decoded at the
/// previous rate, so the measured window sees only the new one.
const SETTLE_BLOCKS: usize = 400;
const MEASURE_BLOCKS: usize = 400;
const FAST_RATE: f32 = 2.0;
const DRAIN_BLOCK_BUDGET: usize = 4_000;
/// The accelerated run must reach EOF within this share of the rate-1.0
/// output. Deliberately far above the ~0.53 the pipeline actually delivers:
/// the discrimination that matters is against an unchanged 1.0 ratio.
const DRAIN_SHARE_NUM: usize = 3;
const DRAIN_SHARE_DEN: usize = 4;

async fn file_resource(
    harness: &OfflinePlayer,
    path: &Path,
    store_dir: &Path,
) -> ResourceConfig<TestPools> {
    let pools = harness.worker().pools().clone();
    let config: ResourceConfig<_> = ResourceConfig::for_src(
        ResourceSrc::parse(path.to_str().expect("utf-8 fixture path"))
            .expect("local media path is a valid resource src"),
    )
    .store(
        AssetStore::builder(pools)
            .backend(StorageBackend::Disk {
                root: store_dir.into(),
            })
            .build(),
    )
    .build();
    config
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
async fn render_blocks(harness: &OfflinePlayer, blocks: usize) {
    for _ in 0..blocks {
        hang_tick!();
        let _ = completed(harness.render(BLOCK_FRAMES)).await;
        let _ = completed(harness.tick_and_drain()).await;
        hang_reset!();
        time::sleep(Duration::from_millis(1)).await;
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn media_advance(harness: &OfflinePlayer, blocks: usize) -> f64 {
    let start = harness.player().position_seconds().unwrap_or(0.0);
    render_blocks(harness, blocks).await;
    hang_reset!();
    harness.player().position_seconds().unwrap_or(0.0) - start
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn blocks_until_end(drain_tone: &'static [u8], temp_dir: &TestTempDir, rate: f32) -> usize {
    let harness = completed(OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    ))
    .await;
    let tag = format!("rate-{rate}");
    let path = temp_dir.path().join(format!("{tag}.wav"));
    std::fs::write(&path, drain_tone).expect("write wav fixture");
    let resource = file_resource(
        &harness,
        &path,
        &temp_dir.path().join(format!("store-{tag}")),
    )
    .await;
    completed(harness.load_and_fadein(resource)).await;
    completed(harness.with_queue(move |player| player.set_default_rate(rate)))
        .await
        .expect("a finite rate is accepted");

    let mut blocks = 0usize;
    let mut ended_at = None;
    let mut previous_position = harness.player().position_seconds();
    for _ in 0..DRAIN_BLOCK_BUDGET {
        hang_tick!();
        let _ = completed(harness.render(BLOCK_FRAMES)).await;
        blocks += 1;
        let ended = completed(harness.tick_and_drain())
            .await
            .iter()
            .any(|event| matches!(event, PlayerEvent::ItemDidPlayToEnd { .. }));
        if ended {
            hang_reset!();
            ended_at = Some(blocks);
            break;
        }
        let position = harness.player().position_seconds();
        if position != previous_position {
            hang_reset!();
            previous_position = position;
        }
        time::sleep(Duration::from_millis(1)).await;
    }
    let blocks = ended_at.unwrap_or_else(|| {
        panic!("the {rate}x track never reached end-of-stream within {DRAIN_BLOCK_BUDGET} blocks")
    });
    completed(harness.close()).await;
    blocks
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn media_time_advances_with_the_playing_rate(tone_mp3: &'static [u8], temp_dir: TestTempDir) {
    let harness = completed(OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    ))
    .await;
    let path = temp_dir.path().join("rate.mp3");
    std::fs::write(&path, tone_mp3).expect("write mp3 fixture");
    let resource = file_resource(&harness, &path, &temp_dir.path().join("store")).await;
    completed(harness.load_and_fadein(resource)).await;

    render_blocks(&harness, SETTLE_BLOCKS).await;
    let baseline = media_advance(&harness, MEASURE_BLOCKS).await;
    assert!(
        baseline > 0.0,
        "precondition: media time must advance at rate 1.0, got {baseline}s"
    );

    completed(harness.with_queue(move |player| player.set_default_rate(FAST_RATE)))
        .await
        .expect("a finite rate is accepted");
    render_blocks(&harness, SETTLE_BLOCKS).await;
    let accelerated = media_advance(&harness, MEASURE_BLOCKS).await;

    assert!(
        accelerated >= baseline * 1.5,
        "over equal rendered-output windows media time advanced \
         {accelerated}s at rate {FAST_RATE} versus {baseline}s at rate 1.0 — \
         the reported clock is on the output scale, not the media scale"
    );
    completed(harness.close()).await;
    hang_reset!();
}

/// The other half of the same contract: the faster media clock has to be
/// backed by the source actually draining faster. Without this, scaling the
/// clock alone would satisfy the trap above while the audio kept its speed.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[kithara_test_utils::kithara::hang_watchdog]
async fn a_faster_rate_drains_the_real_source_sooner(
    drain_tone: &'static [u8],
    temp_dir: TestTempDir,
) {
    let baseline = blocks_until_end(drain_tone, &temp_dir, 1.0).await;
    let accelerated = blocks_until_end(drain_tone, &temp_dir, FAST_RATE).await;

    assert!(
        accelerated * DRAIN_SHARE_DEN <= baseline * DRAIN_SHARE_NUM,
        "at rate {FAST_RATE} the source must reach end-of-stream in \
         materially fewer rendered output blocks than at rate 1.0, got \
         {accelerated} versus {baseline}"
    );
    hang_reset!();
}
