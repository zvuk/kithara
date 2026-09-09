#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{PlayerEvent, TrackId},
    platform::time::{self, Duration},
    play::{Resource, ResourceConfig, ResourceSrc},
};
use kithara_integration_tests::{
    TestTempDir, kithara,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
    temp_dir,
};
use kithara_test_fixtures::{fixtures::tone_mp3, integration_fixtures::drain_tone};

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

async fn file_resource(harness: &OfflinePlayerHarness, path: &Path, store_dir: &Path) -> Resource {
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
    let config = harness
        .player()
        .prepare_config(config)
        .expect("offline player remains open");
    Resource::new(config).await.expect("open local resource")
}

async fn render_blocks(harness: &OfflinePlayerHarness, blocks: usize) {
    for _ in 0..blocks {
        let _ = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
        time::sleep(Duration::from_millis(1)).await;
    }
}

async fn media_advance(harness: &OfflinePlayerHarness, blocks: usize) -> f64 {
    let start = harness.player().position_seconds().unwrap_or(0.0);
    render_blocks(harness, blocks).await;
    harness.player().position_seconds().unwrap_or(0.0) - start
}

async fn blocks_until_end(drain_tone: &'static [u8], temp_dir: &TestTempDir, rate: f32) -> usize {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
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
    harness
        .with_player(move |player| {
            player.insert(resource, TrackId::allocate(), None);
            player
                .select_item(0, true)
                .expect("select first queue item");
        })
        .await;
    harness.player().set_default_rate(rate);

    let mut blocks = 0usize;
    let mut ended_at = None;
    for _ in 0..DRAIN_BLOCK_BUDGET {
        let _ = harness.render(BLOCK_FRAMES).await;
        blocks += 1;
        let ended = harness
            .tick_and_drain()
            .await
            .iter()
            .any(|event| matches!(event, PlayerEvent::ItemDidPlayToEnd { .. }));
        if ended {
            ended_at = Some(blocks);
            break;
        }
        time::sleep(Duration::from_millis(1)).await;
    }
    let blocks = ended_at.unwrap_or_else(|| {
        panic!("the {rate}x track never reached end-of-stream within {DRAIN_BLOCK_BUDGET} blocks")
    });
    harness.close().await;
    blocks
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn media_time_advances_with_the_playing_rate(tone_mp3: &'static [u8], temp_dir: TestTempDir) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let path = temp_dir.path().join("rate.mp3");
    std::fs::write(&path, tone_mp3).expect("write mp3 fixture");
    let resource = file_resource(&harness, &path, &temp_dir.path().join("store")).await;
    harness
        .with_player(move |player| {
            player.insert(resource, TrackId::allocate(), None);
            player
                .select_item(0, true)
                .expect("select first queue item");
        })
        .await;

    render_blocks(&harness, SETTLE_BLOCKS).await;
    let baseline = media_advance(&harness, MEASURE_BLOCKS).await;
    assert!(
        baseline > 0.0,
        "precondition: media time must advance at rate 1.0, got {baseline}s"
    );

    harness.player().set_default_rate(FAST_RATE);
    render_blocks(&harness, SETTLE_BLOCKS).await;
    let accelerated = media_advance(&harness, MEASURE_BLOCKS).await;

    assert!(
        accelerated >= baseline * 1.5,
        "over equal rendered-output windows media time advanced \
         {accelerated}s at rate {FAST_RATE} versus {baseline}s at rate 1.0 — \
         the reported clock is on the output scale, not the media scale"
    );
    harness.close().await;
}

/// The other half of the same contract: the faster media clock has to be
/// backed by the source actually draining faster. Without this, scaling the
/// clock alone would satisfy the trap above while the audio kept its speed.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
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
}
