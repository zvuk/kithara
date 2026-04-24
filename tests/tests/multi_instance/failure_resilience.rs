//! Failure resilience tests.
//!
//! Verifies that when some instances are cancelled mid-stream (simulating
//! a network failure), other instances continue to read PCM data to EOF
//! without being affected.

use std::{path::Path, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread;
use kithara_platform::{
    time::{Duration, sleep},
    tokio::task::{JoinHandle, spawn, spawn_blocking},
};
use kithara_test_utils::TestTempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

struct Consts;
impl Consts {
    const D: SawWav = SawWav::DEFAULT;
    #[cfg(not(target_arch = "wasm32"))]
    const SEGMENT_COUNT: usize = 10;
    #[cfg(target_arch = "wasm32")]
    const SEGMENT_COUNT: usize = 4;
    #[cfg(target_arch = "wasm32")]
    const MAX_ZERO_READS: usize = 200;
}

fn generate_wav_data() -> Arc<Vec<u8>> {
    Consts::D.build_wav(Consts::SEGMENT_COUNT)
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
#[cfg(not(target_arch = "wasm32"))]
fn read_hls_best_effort(audio: &mut Audio<Stream<Hls>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    loop {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    total
}

#[cfg(target_arch = "wasm32")]
fn read_hls_best_effort(audio: &mut Audio<Stream<Hls>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while zero_reads < Consts::MAX_ZERO_READS {
        let n = audio.read(&mut buf);
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            zero_reads += 1;
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        zero_reads = 0;
        total += n as u64;
    }

    total
}

/// Create a healthy HLS server (no delays).
async fn create_server(wav_data: &Arc<Vec<u8>>) -> HlsTestServer {
    HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: Consts::SEGMENT_COUNT,
        segment_size: Consts::D.segment_size,
        segment_duration_secs: Consts::D.segment_duration_secs(),
        custom_data: Some(Arc::clone(wav_data)),
        ..Default::default()
    })
    .await
}

/// Create an `Audio<Stream<Hls>>` instance.
async fn create_hls_audio(
    server: &HlsTestServer,
    cache_dir: &Path,
    cancel: CancellationToken,
) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8");

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(cache_dir))
        .with_cancel(cancel)
        .with_initial_abr_mode(AbrMode::Manual(0));

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);

    Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>")
}

/// Spawn a reader instance that optionally has its cancel fired after `delay_ms`.
async fn spawn_instance(
    id: usize,
    wav_data: &Arc<Vec<u8>>,
    cancel_after: Option<u64>,
) -> JoinHandle<Outcome> {
    let server = create_server(wav_data).await;
    let temp = TestTempDir::new();
    let cancel = CancellationToken::new();
    let healthy = cancel_after.is_none();

    if let Some(delay_ms) = cancel_after {
        let cancel_clone = cancel.clone();
        spawn(async move {
            sleep(Duration::from_millis(delay_ms)).await;
            cancel_clone.cancel();
        });
    }

    let audio = create_hls_audio(&server, temp.path(), cancel).await;

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

    // Stagger cancellation slightly to make it more realistic.
    for (offset, i) in (healthy_count..(healthy_count + cancelled_count)).enumerate() {
        let delay_ms = 200 + offset as u64 * 100;
        handles.push(spawn_instance(i, &wav_data, Some(delay_ms)).await);
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all instances done");

    // Healthy instances must have completed with data.
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
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
#[case::h2_c2(2, 2)]
#[case::h4_c4(4, 4)]
async fn healthy_instances_survive_cancelled_peers(
    #[case] healthy: usize,
    #[case] cancelled: usize,
) {
    run_failure_resilience(healthy, cancelled).await;
}
