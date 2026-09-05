use std::path::Path;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioRead, ReadOutcome},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        CancelToken,
        sync::Arc,
        thread,
        time::{Duration, sleep},
        tokio::task::{JoinHandle, spawn, spawn_blocking},
    },
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir,
    bufpool_ext::{TestPools, pools},
    hls_server::{HlsTestServer, HlsTestServerConfig},
};
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    #[cfg(not(target_arch = "wasm32"))]
    const SEGMENT_COUNT: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const SEGMENT_COUNT: usize = 4;
}

fn generate_wav_data() -> Arc<Vec<u8>> {
    SawWav::DEFAULT.build_wav(Consts::SEGMENT_COUNT)
}

/// Outcome of one instance.
#[derive(Debug)]
struct Outcome {
    id: usize,
    /// `true` = healthy instance, `false` = cancelled instance.
    healthy: bool,
    /// Total samples read (may be partial for cancelled instances).
    total_samples: u64,
}

/// Read HLS audio until EOF or the stream stops producing data.
/// Returns total samples read. Unlike `read_to_eof`, this tolerates
/// early termination because some instances are intentionally cancelled.
fn read_hls_best_effort(audio: &mut RegisteredAudio<Stream<Hls<TestPools>>, TestPools>) -> u64 {
    const MAX_PENDING_READS: usize = if cfg!(target_arch = "wasm32") { 200 } else { 1 };

    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let mut pending_reads = 0usize;

    while pending_reads < MAX_PENDING_READS {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                pending_reads += 1;
                if MAX_PENDING_READS > 1 {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            Ok(ReadOutcome::Frames { count, .. }) => {
                pending_reads = 0;
                total += count.get() as u64;
            }
            Ok(ReadOutcome::Eof { .. }) | Err(_) => break,
        }
    }

    total
}

/// Create a healthy HLS server (no delays).
async fn create_server(wav_data: &Arc<Vec<u8>>) -> HlsTestServer {
    HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: SawWav::DEFAULT.segment_size,
        segment_duration_secs: SawWav::DEFAULT.segment_duration_secs(),
        custom_data: Some(Arc::clone(wav_data)),
        ..Default::default()
    })
    .await
}

/// Create an `Audio<Stream<Hls>>` instance.
async fn create_hls_audio(
    server: &HlsTestServer,
    cache_dir: &Path,
    cancel: CancelToken,
) -> RegisteredAudio<Stream<Hls<TestPools>>, TestPools> {
    let url = server.url("/master.m3u8");
    let pools = pools();

    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: cache_dir.into(),
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
    worker
        .open(config)
        .await
        .expect("create Audio<Stream<Hls>>")
}

/// Spawn a reader instance whose cancel, when `cancel_after` is set, fires
/// `delay_ms` after creation completes — modelling a peer cancelled mid
/// playback. The timer is armed only once `create_hls_audio` returns so a
/// slow create under load cannot race the cancel into `PlayWorker::open` and
/// surface as `source error: cancelled` from creation itself.
async fn spawn_instance(
    id: usize,
    wav_data: &Arc<Vec<u8>>,
    cancel_after: Option<u64>,
) -> JoinHandle<Outcome> {
    let server = create_server(wav_data).await;
    let temp = TestTempDir::new();
    let cancel = CancelToken::never();
    let healthy = cancel_after.is_none();

    let audio = create_hls_audio(&server, temp.path(), cancel.clone()).await;

    if let Some(delay_ms) = cancel_after {
        let cancel_clone = cancel.clone();
        spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            cancel_clone.cancel();
        });
    }

    spawn_blocking(move || {
        let _server = server;
        let _temp = temp;
        let mut audio = audio;
        let total = read_hls_best_effort(&mut audio);
        info!(
            instance = id,
            total_samples = total,
            healthy,
            "instance done",
        );
        Outcome {
            id,
            healthy,
            total_samples: total,
        }
    })
}

async fn run_failure_resilience(healthy_count: usize, cancelled_count: usize) {
    let wav_data = generate_wav_data();
    let mut handles: Vec<JoinHandle<Outcome>> = Vec::new();

    for i in 0..healthy_count {
        handles.push(spawn_instance(i, &wav_data, None).await);
    }

    for (offset, i) in (healthy_count..(healthy_count + cancelled_count)).enumerate() {
        let delay_ms = 200 + offset as u64 * 100;
        handles.push(spawn_instance(i, &wav_data, Some(delay_ms)).await);
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all instances done");

    for r in results.iter().filter(|r| r.healthy) {
        assert!(
            r.total_samples > 0,
            "healthy instance {} read 0 samples",
            r.id
        );
        info!(
            instance = r.id,
            total_samples = r.total_samples,
            "healthy instance verified"
        );
    }

    let healthy_min = results
        .iter()
        .filter(|r| r.healthy)
        .map(|r| r.total_samples)
        .min()
        .unwrap_or(0);

    for r in results.iter().filter(|r| !r.healthy) {
        info!(
            instance = r.id,
            total_samples = r.total_samples,
            healthy_min,
            "cancelled instance outcome"
        );
    }
}

/// Healthy + cancelled HLS instance mixes. Cancelled peers must not harm
/// healthy ones (which must still reach EOF).
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(2),
    tracing("kithara_hls=debug,kithara_stream=debug")
)]
#[case::h2_c2(2, 2)]
#[case::h4_c4(4, 4)]
async fn healthy_instances_survive_cancelled_peers(
    #[case] healthy: usize,
    #[case] cancelled: usize,
) {
    run_failure_resilience(healthy, cancelled).await;
}
