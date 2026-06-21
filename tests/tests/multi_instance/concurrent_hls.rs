use std::{path::Path, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir, auto,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    reads::{ReadLimit, read_for_concurrency_check},
};
use kithara_platform::{CancelToken, time::Duration, tokio::task::spawn_blocking};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    #[cfg(not(target_arch = "wasm32"))]
    const SEGMENT_COUNT: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const SEGMENT_COUNT: usize = 4;
}

/// Create an HLS server; `abr_variants == 1` → single variant, otherwise ABR.
async fn create_hls_server(wav_data: Arc<Vec<u8>>, abr_variants: usize) -> HlsTestServer {
    let config = HlsTestServerConfig {
        variant_count: abr_variants,
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: SawWav::DEFAULT.segment_size,
        segment_duration_secs: SawWav::DEFAULT.segment_duration_secs(),
        custom_data: Some(wav_data),
        variant_bandwidths: (abr_variants > 1).then(|| vec![5_000_000, 1_000_000]),
        ..Default::default()
    };
    HlsTestServer::new(config).await
}

/// Create an `Audio<Stream<Hls>>` for `abr` mode (Manual(0) or Auto(Some(0))).
async fn create_hls_audio(
    server: &HlsTestServer,
    cache_dir: &Path,
    abr: AbrMode,
) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8");
    let cancel = CancelToken::never();

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(cache_dir))
        .cancel(cancel)
        .initial_abr_mode(abr)
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    // Park on ring underrun instead of surfacing Pending, so the blocking
    // readers never spin against the virtual clock.
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(wav_info)
        .block_on_underrun(true)
        .build();

    Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>")
}

fn generate_wav_data() -> Arc<Vec<u8>> {
    SawWav::DEFAULT.build_wav(Consts::SEGMENT_COUNT)
}

/// Spawn `n` concurrent HLS readers and assert each reads non-zero samples.
async fn run_concurrent_hls(n: usize, abr: AbrMode, variants: usize) {
    let wav_data = generate_wav_data();

    let mut handles = Vec::new();
    for i in 0..n {
        let server = create_hls_server(Arc::clone(&wav_data), variants).await;
        let temp = TestTempDir::new();
        let audio = create_hls_audio(&server, temp.path(), abr).await;
        handles.push(spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio, ReadLimit::wasm_default());
            info!(instance = i, total_samples = total, "instance finished");
            (i, total)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all HLS instances done");
    for (id, total) in &results {
        assert!(*total > 0, "HLS instance {id} read 0 samples");
    }
}

#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
#[case::n2_manual(2, AbrMode::manual(0), 1)]
#[case::n4_manual(4, AbrMode::manual(0), 1)]
#[case::n8_manual(8, AbrMode::manual(0), 1)]
#[case::n4_auto_abr(4, auto(0), 2)]
async fn concurrent_hls_instances(
    #[case] instances: usize,
    #[case] abr: AbrMode,
    #[case] variants: usize,
) {
    run_concurrent_hls(instances, abr, variants).await;
}
