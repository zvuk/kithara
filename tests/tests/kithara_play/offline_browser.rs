//! Offline render through the product graph, measured the same way on the
//! browser and on native.

use kithara::{
    assets::{AssetStore, StorageBackend},
    host::HostConfig,
    platform::time::Duration,
    play::{Resource, ResourceConfig, ResourceSrc},
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    offline::{OfflinePlayer, OfflineWorker, max_silence_run, peak, rms},
};
use kithara_test_fixtures::SignalAsset;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: usize = 2;
const BLOCK_FRAMES: usize = 512;
const WARMUP_BLOCKS: usize = 8;
const MEASURE_BLOCKS: usize = 32;
const TAP_CAPACITY: usize = 65_536;
const SILENCE_THRESHOLD: f32 = 0.001;
/// The signal route serves every tone at full scale, so a sine has RMS
/// `1/sqrt(2)`; the band leaves room for the limiter and mp3 framing.
const MIN_RMS: f32 = 0.55;
const MAX_RMS: f32 = 0.75;

/// One offline player on the fixture sine, playing with no fade. Everything
/// that waits on the fixture bytes is opened on the harness thread.
async fn playing_worker() -> OfflineWorker {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_SINE440_60S);
    let region = pools();
    let host = HostConfig::offline(region.clone())
        .sample_rate(SAMPLE_RATE.try_into().expect("the sample rate is non-zero"))
        .build();
    let worker = OfflineWorker::new(async move || OfflinePlayer::new(host).await).await;
    worker
        .call(async move |player| {
            let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
                ResourceSrc::parse(url.as_str())
                    .expect("the fixture URL parses as a resource source"),
            )
            .worker(player.worker().clone())
            .store(
                AssetStore::builder(region)
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build();
            let mut resource = Resource::new(config)
                .await
                .expect("open the fixture as a product resource");
            resource.preload().await.expect("preload the fixture");
            player.set_fade_duration(0.0);
            player.load_and_fadein(resource).await;
        })
        .await;
    worker
}

async fn render_blocks(worker: &OfflineWorker, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES * CHANNELS);
    for _ in 0..blocks {
        let block = worker
            .call(async move |player| player.render(BLOCK_FRAMES).await)
            .await;
        rendered.extend_from_slice(&block);
    }
    rendered
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(1)
)]
async fn offline_render_carries_fixture_signal() {
    let worker = playing_worker().await;
    let _warmup = render_blocks(&worker, WARMUP_BLOCKS).await;
    let measured = render_blocks(&worker, MEASURE_BLOCKS).await;

    assert_eq!(measured.len(), MEASURE_BLOCKS * BLOCK_FRAMES * CHANNELS);
    let level = rms(&measured);
    assert!(
        (MIN_RMS..=MAX_RMS).contains(&level),
        "full-scale sine renders at RMS {level:.4}, outside {MIN_RMS}..={MAX_RMS}"
    );
    let ceiling = peak(&measured);
    assert!(ceiling <= 1.0, "render clipped at peak {ceiling:.4}");
    let silence = max_silence_run(&measured, 0, measured.len(), SILENCE_THRESHOLD);
    assert!(
        silence < BLOCK_FRAMES * CHANNELS,
        "render went silent for {silence} samples, a whole block or more"
    );
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(1)
)]
async fn offline_mix_tap_mirrors_render() {
    let worker = playing_worker().await;
    let _warmup = render_blocks(&worker, WARMUP_BLOCKS).await;
    let mut tap = worker
        .call(async move |player| player.host().enable_mix_tap(TAP_CAPACITY).await)
        .await
        .expect("enable the mix tap");

    let rendered = render_blocks(&worker, MEASURE_BLOCKS).await;
    let tapped = tap.drain();

    assert_eq!(tapped, rendered, "the tap carries what the sink received");
    assert_eq!(tap.drops(), 0);
}
