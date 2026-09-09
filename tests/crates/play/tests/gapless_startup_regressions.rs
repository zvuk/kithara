#![cfg(not(target_arch = "wasm32"))]

use std::{num::NonZeroU32, path::Path};

use kithara::{
    decode::{GaplessMode, SilenceTrimParams},
    events::TrackId,
    platform::time::{Duration, Instant},
    play::{Resource, ResourceConfig, ResourceSrc, player::PlayerControl},
    stream::AudioCodec,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{
        DelayRule, GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
    temp_dir,
};
use url::Url;

use crate::{
    bufpool_ext::TestPools,
    gapless_common::{
        AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_TRAILING_DELAY,
        GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
    },
};

const BLOCK_FRAMES: usize = 512;
const DELAYED_SEGMENT_INDEX: usize = 2;
const DELAY_MS: u64 = 2_500;
const SEGMENTS_PER_VARIANT: usize = 6;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(4);
const STARTUP_POSITION_SECS: f64 = 0.05;
const AUDIBLE_SAMPLE_THRESHOLD: f32 = 1.0e-3;

#[kithara::test(native, tokio, timeout(Duration::from_secs(20)), hang_timeout_secs(1))]
#[case(GaplessMode::MediaOnly)]
#[case(GaplessMode::CodecPriming)]
#[case(GaplessMode::SilenceTrim(SilenceTrimParams::default()))]
async fn gapless_modes_do_not_block_network_startup_until_full_cache(
    #[future(awt)] startup_source: (TestServerHelper, Url),
    #[case] gapless_mode: GaplessMode,
    temp_dir: TestTempDir,
) {
    let (_server, master) = startup_source;
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .gapless_mode(gapless_mode)
            .build(),
        GAPLESS_SAMPLE_RATE,
    )
    .await;
    let resource = create_delayed_gapless_hls_resource(&harness, &master, temp_dir.path()).await;

    harness
        .with_player(move |player| player.insert(resource, TrackId::allocate(), None))
        .await;

    let started_at = Instant::now();
    harness.with_player(PlayerControl::play).await;
    let _ = harness.tick_and_drain().await;

    let deadline = started_at + STARTUP_TIMEOUT;
    let mut rendered = Vec::new();

    loop {
        let block = harness.render(BLOCK_FRAMES).await;
        let _ = harness.tick_and_drain().await;
        rendered.extend_from_slice(&block);

        let position = harness.player().position_seconds().unwrap_or(0.0);
        let audible = rendered
            .iter()
            .any(|sample| sample.abs() > AUDIBLE_SAMPLE_THRESHOLD);

        if audible && position > STARTUP_POSITION_SECS {
            let elapsed = started_at.elapsed();
            assert!(
                elapsed < Duration::from_millis(DELAY_MS),
                "gapless mode {gapless_mode:?} must start before delayed tail segments \
                could fully cache; elapsed={elapsed:?}, position={position:.3}s"
            );
            harness.close().await;
            return;
        }

        assert!(
            Instant::now() <= deadline,
            "timed out waiting for startup with gapless mode {gapless_mode:?}; \
             position={position:.3}s, rendered_samples={}",
            rendered.len()
        );
        time::sleep(Duration::from_millis(10)).await;
    }
}

async fn create_delayed_gapless_hls_resource(
    harness: &OfflinePlayerHarness,
    master: &Url,
    cache_dir: &Path,
) -> Resource {
    let store = kithara_integration_tests::disk_asset_store(cache_dir);
    let mut config = ResourceConfig::<TestPools>::for_src(
        ResourceSrc::parse(master.as_str()).expect("valid HLS master URL"),
    )
    .store(store)
    .build();
    config = harness
        .with_player(move |player| player.prepare_config(config))
        .await
        .expect("prepare delayed gapless HLS resource config");

    Resource::new(config)
        .await
        .expect("open delayed gapless HLS resource")
}

#[kithara::fixture]
async fn startup_source() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(SEGMENTS_PER_VARIANT)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .delay_rules(vec![DelayRule {
                    variant: Some(0),
                    segment_gte: Some(DELAYED_SEGMENT_INDEX),
                    delay_ms: DELAY_MS,
                    ..Default::default()
                }])
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: None,
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: NonZeroU32::new(AAC_GAPLESS_ENCODER_DELAY),
                    trailing_delay: NonZeroU32::new(AAC_GAPLESS_TRAILING_DELAY),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: 880.0 }),
                    gapless_encoding: GaplessEncoding::default(),
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create delayed gapless HLS fixture");

    (server, created.master_url())
}
