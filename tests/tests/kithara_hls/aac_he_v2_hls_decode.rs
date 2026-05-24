//! AAC HE v2 (Parametric Stereo) end-to-end decode contract.
//!
//! Production zvuk.com serves the `slq` variant as `mp4a.40.29` (HE-AAC v2
//! with SBR + PS), and macOS playback was observed to silently fail —
//! `AudioConverterSetProperty(MagicCookie)` returned a non-zero status
//! and every subsequent decode call emitted `AudioCodecBadDataError`
//! (`'bada'`). The synthetic AAC LC + FLAC fixtures play fine because
//! the AAC HE v2 specific cookie path is never exercised.
//!
//! This test wires the full HLS pipeline (`Stream<Hls>` + `Audio<…>`)
//! against an `HlsFixtureBuilder.packaged_audio_sine_aac_he_v2(…, 440 Hz)`
//! stream and checks the decoded PCM's fundamental frequency via
//! zero-crossing density — Apple-side regressions show up as silence,
//! channel duplication, or wrong sample rate, all of which derail the
//! estimate far past the ±15 Hz tolerance band.

#![forbid(unsafe_code)]

use std::time::Duration;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::{HlsFixtureBuilder, TestServerHelper, TestTempDir, temp_dir};
use kithara_platform::tokio::task::spawn_blocking;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(15)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn aac_he_v2_hls_produces_pcm(temp_dir: TestTempDir, #[case] backend: DecoderBackend) {
    let server = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(8)
        .segment_duration_secs(0.5)
        .packaged_audio_aac_he_v2(SAMPLE_RATE, CHANNELS);
    let created = server
        .create_hls(builder)
        .await
        .expect("create AAC HE v2 HLS fixture");

    let hls_config = HlsConfig::for_url(created.master_url())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(backend)
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");

    let pcm = spawn_blocking(move || {
        let target_samples = SAMPLE_RATE as usize * CHANNELS as usize;
        let mut collected: Vec<f32> = Vec::with_capacity(target_samples);
        let mut buf = vec![0f32; 16384];
        for _ in 0..2000 {
            if collected.len() >= target_samples {
                break;
            }
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    collected.extend_from_slice(&buf[..count.get()]);
                }
                Ok(ReadOutcome::Pending { .. }) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("HE-AAC v2 decode error: {e}"),
            }
        }
        collected
    })
    .await
    .expect("spawn_blocking");

    assert!(
        pcm.len() >= SAMPLE_RATE as usize,
        "HE-AAC v2 decoded too few PCM samples: got {} (want >= {} for ~0.5 s of stereo)",
        pcm.len(),
        SAMPLE_RATE as usize
    );

    let nonzero = pcm.iter().filter(|s| s.abs() > 1e-6).count();
    assert!(
        nonzero >= pcm.len() / 4,
        "HE-AAC v2 PCM looks like silence: {nonzero}/{} non-zero samples",
        pcm.len()
    );
}
