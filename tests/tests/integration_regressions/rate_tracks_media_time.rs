#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    events::PlayerEvent,
    platform::time::{self, Duration},
    play::{PlayerImpl, Resource, ResourceConfig},
};
use kithara_integration_tests::{
    TestTempDir, audio_fixture::EmbeddedAudio, create_test_wav, disk_asset_store, kithara, temp_dir,
};

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
/// Long enough for the ring to have flushed every block decoded at the
/// previous rate, so the measured window sees only the new one.
const SETTLE_BLOCKS: usize = 400;
const MEASURE_BLOCKS: usize = 400;
const FAST_RATE: f32 = 2.0;
const DRAIN_FIXTURE_SECS: usize = 4;
const DRAIN_BLOCK_BUDGET: usize = 4_000;
/// The accelerated run must reach EOF within this share of the rate-1.0
/// output. Deliberately far above the ~0.53 the pipeline actually delivers:
/// the discrimination that matters is against an unchanged 1.0 ratio.
const DRAIN_SHARE_NUM: usize = 3;
const DRAIN_SHARE_DEN: usize = 4;

async fn file_resource(player: &PlayerImpl, path: &Path, store_dir: &Path) -> Resource {
    let config = ResourceConfig::for_src(path.to_str().expect("utf-8 fixture path"))
        .expect("local media path is a valid resource src")
        .store(disk_asset_store(store_dir))
        .byte_pool(player.byte_pool().clone())
        .pcm_pool(player.pcm_pool().clone())
        .build();
    Resource::new(player.prepare_config(config))
        .await
        .expect("open local resource")
}

async fn render_blocks(harness: &OfflinePlayerHarness, blocks: usize) {
    for _ in 0..blocks {
        let _ = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
        time::sleep(Duration::from_millis(1)).await;
    }
}

async fn media_advance(harness: &OfflinePlayerHarness, blocks: usize) -> f64 {
    let start = harness.player().position_seconds().unwrap_or(0.0);
    render_blocks(harness, blocks).await;
    harness.player().position_seconds().unwrap_or(0.0) - start
}

async fn blocks_until_end(temp_dir: &TestTempDir, rate: f32) -> usize {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let tag = format!("rate-{rate}");
    let path = temp_dir.path().join(format!("{tag}.wav"));
    let frames = DRAIN_FIXTURE_SECS * SAMPLE_RATE as usize;
    std::fs::write(&path, create_test_wav(frames, SAMPLE_RATE, 2)).expect("write wav fixture");
    let resource = file_resource(
        harness.player(),
        &path,
        &temp_dir.path().join(format!("store-{tag}")),
    )
    .await;
    harness.player().insert(resource, None, None);
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");
    harness.player().set_default_rate(rate);

    let mut blocks = 0usize;
    for _ in 0..DRAIN_BLOCK_BUDGET {
        let _ = harness.render(BLOCK_FRAMES);
        blocks += 1;
        let ended = harness
            .tick_and_drain()
            .iter()
            .any(|event| matches!(event, PlayerEvent::ItemDidPlayToEnd { .. }));
        if ended {
            return blocks;
        }
        time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the {rate}x track never reached end-of-stream within {DRAIN_BLOCK_BUDGET} blocks");
}

/// A rate change during playback must move media time, not just the
/// reported rate.
///
/// The mock-reader trap next door drives `PcmReader::set_playback_rate` on a
/// test reader that reimplements the speed itself, so it never exercised the
/// production chain. This one runs a real decoded file through the real
/// time-stretch slot and asks what the iOS surface asks: over equal
/// rendered-output windows, does the reported position advance faster?
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn media_time_advances_with_the_playing_rate(temp_dir: TestTempDir) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let path = temp_dir.path().join("rate.mp3");
    std::fs::write(&path, EmbeddedAudio::TEST_MP3_BYTES).expect("write mp3 fixture");
    let resource = file_resource(harness.player(), &path, &temp_dir.path().join("store")).await;
    harness.player().insert(resource, None, None);
    harness
        .player()
        .select_item(0, true)
        .expect("select first queue item");

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
}

/// The other half of the same contract: the faster media clock has to be
/// backed by the source actually draining faster. Without this, scaling the
/// clock alone would satisfy the trap above while the audio kept its speed.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn a_faster_rate_drains_the_real_source_sooner(temp_dir: TestTempDir) {
    let baseline = blocks_until_end(&temp_dir, 1.0).await;
    let accelerated = blocks_until_end(&temp_dir, FAST_RATE).await;

    assert!(
        accelerated * DRAIN_SHARE_DEN <= baseline * DRAIN_SHARE_NUM,
        "at rate {FAST_RATE} the source must reach end-of-stream in \
         materially fewer rendered output blocks than at rate 1.0, got \
         {accelerated} versus {baseline}"
    );
}
