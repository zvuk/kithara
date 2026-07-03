#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    hls::{Hls, HlsConfig},
    platform::{thread, time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{HlsFixtureBuilder, TestServerHelper, TestTempDir, temp_dir};

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
    // Park on ring underrun instead of spinning on Pending, so the read
    // loop needs no wall-clock iteration cap.
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(backend)
        .block_on_underrun(true)
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");

    let pcm = spawn_blocking(move || {
        let target_samples = SAMPLE_RATE as usize * CHANNELS as usize;
        let mut collected: Vec<f32> = Vec::with_capacity(target_samples);
        let mut buf = vec![0f32; 16384];
        loop {
            if collected.len() >= target_samples {
                break;
            }
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    collected.extend_from_slice(&buf[..count.get()]);
                }
                Ok(ReadOutcome::Pending { .. }) => {
                    thread::sleep(Duration::from_millis(5));
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
