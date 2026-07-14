#![forbid(unsafe_code)]

use std::{fs, path::Path};

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

#[kithara::test(tokio, native, timeout(Duration::from_secs(30)))]
#[ignore = "regenerates committed HE-AAC v1 browser fixtures"]
async fn generate_he_aac_v1_fixture() {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(1)
                .segment_duration_secs(1.0)
                .packaged_audio_aac_he(SAMPLE_RATE, CHANNELS),
        )
        .await
        .expect("create HE-AAC v1 fixture");
    let init = reqwest::get(created.init_url(0))
        .await
        .expect("request HE-AAC v1 init segment")
        .error_for_status()
        .expect("HE-AAC v1 init segment status")
        .bytes()
        .await
        .expect("read HE-AAC v1 init segment");
    let segment = reqwest::get(created.segment_url(0, 0))
        .await
        .expect("request HE-AAC v1 media segment")
        .error_for_status()
        .expect("HE-AAC v1 media segment status")
        .bytes()
        .await
        .expect("read HE-AAC v1 media segment");
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets");

    fs::write(assets.join("he_aac_v1_init.mp4"), init).expect("write HE-AAC v1 init fixture");
    fs::write(assets.join("he_aac_v1_segment.m4s"), segment)
        .expect("write HE-AAC v1 media fixture");
}

#[kithara::test(tokio, native, timeout(Duration::from_secs(30)))]
#[ignore = "regenerates committed HE-AAC v2 browser fixtures"]
async fn generate_he_aac_v2_fixture() {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(1)
                .segment_duration_secs(1.0)
                .packaged_audio_aac_he_v2(SAMPLE_RATE, CHANNELS),
        )
        .await
        .expect("create HE-AAC v2 fixture");
    let init = reqwest::get(created.init_url(0))
        .await
        .expect("request HE-AAC v2 init segment")
        .error_for_status()
        .expect("HE-AAC v2 init segment status")
        .bytes()
        .await
        .expect("read HE-AAC v2 init segment");
    let segment = reqwest::get(created.segment_url(0, 0))
        .await
        .expect("request HE-AAC v2 media segment")
        .error_for_status()
        .expect("HE-AAC v2 media segment status")
        .bytes()
        .await
        .expect("read HE-AAC v2 media segment");
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets");

    fs::write(assets.join("he_aac_v2_init.mp4"), init).expect("write HE-AAC v2 init fixture");
    fs::write(assets.join("he_aac_v2_segment.m4s"), segment)
        .expect("write HE-AAC v2 media fixture");
}

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
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
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
