#![cfg(not(target_arch = "wasm32"))]

use std::{num::NonZeroU32, path::Path};

use kithara_assets::StoreOptions;
use kithara_decode::{GaplessMode, SilenceTrimParams};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_play::{PlayerConfig, Resource, ResourceConfig};
use kithara_stream::AudioCodec;
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{
        DelayRule, GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
    temp_dir,
};

use super::offline_player_harness::OfflinePlayerHarness;
use crate::gapless_common::{
    AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_TRAILING_DELAY,
    GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
};

const BLOCK_FRAMES: usize = 512;
const DELAYED_SEGMENT_INDEX: usize = 2;
const DELAY_MS: u64 = 2_500;
const SEGMENTS_PER_VARIANT: usize = 6;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(4);
const STARTUP_POSITION_SECS: f64 = 0.05;
const AUDIBLE_SAMPLE_THRESHOLD: f32 = 1.0e-3;

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(GaplessMode::MediaOnly)]
#[case(GaplessMode::CodecPriming)]
#[case(GaplessMode::SilenceTrim(SilenceTrimParams::default()))]
async fn gapless_modes_do_not_block_network_startup_until_full_cache(
    #[case] gapless_mode: GaplessMode,
    temp_dir: TestTempDir,
) {
    let server = TestServerHelper::new().await;
    let harness = OfflinePlayerHarness::with_sample_rate(
        PlayerConfig::default().with_gapless_mode(gapless_mode),
        GAPLESS_SAMPLE_RATE,
    );
    let resource =
        create_delayed_gapless_hls_resource(harness.player(), &server, temp_dir.path()).await;

    harness.player().insert(resource, None, None);

    let started_at = Instant::now();
    harness.player().play();
    let _ = harness.tick_and_drain();

    let deadline = started_at + STARTUP_TIMEOUT;
    let mut rendered = Vec::new();

    loop {
        let block = harness.render(BLOCK_FRAMES);
        let _ = harness.tick_and_drain();
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
            return;
        }

        assert!(
            Instant::now() <= deadline,
            "timed out waiting for startup with gapless mode {gapless_mode:?}; \
             position={position:.3}s, rendered_samples={}",
            rendered.len()
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn create_delayed_gapless_hls_resource(
    player: &kithara_play::PlayerImpl,
    server: &TestServerHelper,
    cache_dir: &Path,
) -> Resource {
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

    let store = StoreOptions::new(cache_dir);
    let mut config = ResourceConfig::new(created.master_url().as_str())
        .expect("valid HLS master URL")
        .with_store(store);
    player.prepare_config(&mut config);

    Resource::new(config)
        .await
        .expect("open delayed gapless HLS resource")
}
