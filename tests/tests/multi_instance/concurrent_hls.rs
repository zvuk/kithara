//! Concurrent HLS instance tests.
//!
//! Verifies that N `Audio<Stream<Hls>>` instances can run concurrently and
//! each reads PCM data to EOF, under manual-variant and auto-ABR modes.

use std::{path::Path, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::TestTempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::{
    reader_helpers::{ReadLimit, read_for_concurrency_check},
    test_defaults::SawWav,
};

struct Consts;
impl Consts {
    #[cfg(not(target_arch = "wasm32"))]
    const SEGMENT_COUNT: usize = 10; // Smaller than stress test — enough for concurrency check.
    #[cfg(target_arch = "wasm32")]
    const SEGMENT_COUNT: usize = 4; // Keep fixture session payload small in browser-runner tests.
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
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(cache_dir))
        .with_cancel(cancel)
        .with_initial_abr_mode(abr);

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

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
        // Each instance needs its own server (binds a random port) and cache dir.
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
#[case::n2_manual(2, AbrMode::Manual(0), 1)]
#[case::n4_manual(4, AbrMode::Manual(0), 1)]
#[case::n8_manual(8, AbrMode::Manual(0), 1)]
#[case::n4_auto_abr(4, AbrMode::Auto(Some(0)), 2)]
async fn concurrent_hls_instances(
    #[case] instances: usize,
    #[case] abr: AbrMode,
    #[case] variants: usize,
) {
    run_concurrent_hls(instances, abr, variants).await;
}
