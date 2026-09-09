use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::AudioConfig,
    file::{File, FileConfig},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        CancelToken,
        sync::Arc,
        time::Duration,
        tokio::task::{JoinHandle, spawn_blocking},
    },
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    hls_server::{HlsTestServer, HlsTestServerConfig},
    reads::{ReadLimit, read_for_concurrency_check},
};
use kithara_test_fixtures::{SignalAsset, integration_fixtures::concurrent_wav};
use tracing::info;
use url::Url;

use crate::common::test_defaults::SawWav;

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

async fn spawn_file_instance(
    id: usize,
    url: Url,
    temp_path: &std::path::Path,
) -> JoinHandle<InstanceResult> {
    let pools = pools();
    let file_config = FileConfig::for_src(url.into())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_path.into(),
                })
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .hint(("mp3").to_string())
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let mut audio = worker.open(config).await.expect("create File audio");

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
    let cancel = CancelToken::never();
    let pools = pools();

    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_path.into(),
                })
                .build(),
        )
        .pools(pools.clone())
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let wav_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
        .media_info(wav_info)
        .build();

    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let mut audio = worker.open(config).await.expect("create HLS audio");

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

async fn run_mixed(
    source: (TestServerHelper, Url),
    concurrent_wav: &'static [u8],
    file_count: usize,
    hls_count: usize,
) {
    let wav_data = Arc::new(concurrent_wav.to_vec());
    let (_file_server, file_url) = source;

    let mut handles: Vec<JoinHandle<InstanceResult>> = Vec::new();
    let mut temps = Vec::new();
    let mut servers = Vec::new();

    for i in 0..file_count {
        let temp = TestTempDir::new();
        let h = spawn_file_instance(i, file_url.clone(), temp.path()).await;
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
    hang_timeout_secs(2)
)]
#[case::two_file_two_hls(2, 2)]
#[case::four_file_four_hls(4, 4)]
async fn concurrent_mixed_instances(
    concurrent_wav: &'static [u8],
    #[future(awt)] file_source: (TestServerHelper, Url),
    #[case] file_count: usize,
    #[case] hls_count: usize,
) {
    run_mixed(file_source, concurrent_wav, file_count, hls_count).await;
}

#[kithara::fixture]
async fn file_source() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_TRACK_SINE440_187S);
    (server, url)
}
