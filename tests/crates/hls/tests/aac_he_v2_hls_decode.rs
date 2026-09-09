#![forbid(unsafe_code)]

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioRead, ReadOutcome},
    decode::DecoderBackend,
    hls::{Hls, HlsConfig},
    platform::{thread, time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    temp_dir,
};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;

#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(15)),
    hang_timeout_secs(3)
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn aac_he_v2_hls_produces_pcm(
    temp_dir: TestTempDir,
    #[case] backend: DecoderBackend,
    #[future(awt)] he_hls: (TestServerHelper, CreatedHls),
) {
    let (_server, created) = he_hls;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let hls_config = HlsConfig::for_url(created.master_url())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .build();
    // Park on ring underrun instead of spinning on Pending, so the read
    // loop needs no wall-clock iteration cap.
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .block_on_underrun(true)
        .build();

    let mut audio = worker.open(config).await.expect("audio creation");

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

#[kithara::fixture]
async fn he_hls() -> (TestServerHelper, CreatedHls) {
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

    (server, created)
}
