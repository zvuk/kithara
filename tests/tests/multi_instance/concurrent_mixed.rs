//! Mixed concurrent File + HLS instance tests.
//!
//! Verifies that File and HLS `Audio` instances can coexist and all
//! read PCM data to EOF.

use std::sync::Arc;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{
    time::Duration,
    tokio::task::{JoinHandle, spawn_blocking},
};
use kithara_test_utils::{TestServerHelper, TestTempDir};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::{
    reader_helpers::{ReadLimit, read_for_concurrency_check},
    test_defaults::SawWav,
};

struct Consts;
impl Consts {
    #[cfg(not(target_arch = "wasm32"))]
    const SEGMENT_COUNT: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const SEGMENT_COUNT: usize = 4;
}

/// Result of one instance completing.
#[derive(Debug)]
struct InstanceResult {
    id: usize,
    kind: &'static str,
    total_samples: u64,
}

fn generate_wav_data() -> Arc<Vec<u8>> {
    SawWav::DEFAULT.build_wav(Consts::SEGMENT_COUNT)
}

async fn spawn_file_instance(
    id: usize,
    url: url::Url,
    temp_path: &std::path::Path,
) -> JoinHandle<InstanceResult> {
    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_path));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create File audio");

    spawn_blocking(move || {
        let total = read_for_concurrency_check(&mut audio, ReadLimit::wasm_default());
        info!(instance = id, kind = "file", total_samples = total, "done");
        InstanceResult {
            id,
            kind: "file",
            total_samples: total,
        }
    })
}

async fn spawn_hls_instance(
    id: usize,
    wav_data: Arc<Vec<u8>>,
    temp_path: &std::path::Path,
) -> (HlsTestServer, JoinHandle<InstanceResult>) {
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: SawWav::DEFAULT.segment_size,
        segment_duration_secs: SawWav::DEFAULT.segment_duration_secs(),
        custom_data: Some(wav_data),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_path))
        .with_cancel(cancel)
        .with_initial_abr_mode(AbrMode::Manual(0));

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create HLS audio");

    let handle = spawn_blocking(move || {
        let total = read_for_concurrency_check(&mut audio, ReadLimit::wasm_default());
        info!(instance = id, kind = "hls", total_samples = total, "done");
        InstanceResult {
            id,
            kind: "hls",
            total_samples: total,
        }
    });

    (server, handle)
}

async fn run_mixed(file_count: usize, hls_count: usize) {
    let wav_data = generate_wav_data();
    let file_server = TestServerHelper::new().await;

    let mut handles: Vec<JoinHandle<InstanceResult>> = Vec::new();
    let mut temps = Vec::new();
    let mut servers = Vec::new();

    for i in 0..file_count {
        let temp = TestTempDir::new();
        let h = spawn_file_instance(i, file_server.asset("test.mp3"), temp.path()).await;
        temps.push(temp);
        handles.push(h);
    }

    for i in file_count..(file_count + hls_count) {
        let temp = TestTempDir::new();
        let (server, h) = spawn_hls_instance(i, Arc::clone(&wav_data), temp.path()).await;
        temps.push(temp);
        servers.push(server);
        handles.push(h);
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }
    drop(temps);
    drop(servers);

    info!(?results, "all mixed instances done");
    for r in &results {
        assert!(
            r.total_samples > 0,
            "instance {} ({}) read 0 samples",
            r.id,
            r.kind
        );
    }
}

/// Mixed File + HLS instances running concurrently.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
#[case::two_file_two_hls(2, 2)]
#[case::four_file_four_hls(4, 4)]
async fn concurrent_mixed_instances(#[case] file_count: usize, #[case] hls_count: usize) {
    run_mixed(file_count, hls_count).await;
}
