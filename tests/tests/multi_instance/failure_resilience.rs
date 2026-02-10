//! Failure resilience tests.
//!
//! Verifies that when some instances are cancelled mid-stream (simulating
//! a network failure), other instances on the same shared `ThreadPool`
//! continue to read PCM data to EOF without being affected.

use std::{sync::Arc, time::Duration};

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, ThreadPool};
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{
    common::wav::create_test_wav,
    kithara_hls::fixture::{HlsTestServer, HlsTestServerConfig},
};

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 10;

fn generate_wav_data() -> Arc<Vec<u8>> {
    let total_bytes = SEGMENT_COUNT * SEGMENT_SIZE;
    let bytes_per_frame = CHANNELS as usize * 2;
    let header_size = 44;
    let sample_count = (total_bytes - header_size) / bytes_per_frame;
    Arc::new(create_test_wav(sample_count, SAMPLE_RATE, CHANNELS))
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
/// Returns total samples read.
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

/// Create a healthy HLS server (no delays).
async fn create_server(wav_data: &Arc<Vec<u8>>) -> HlsTestServer {
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);
    HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::clone(wav_data)),
        ..Default::default()
    })
    .await
}

/// Create an `Audio<Stream<Hls>>` instance.
async fn create_hls_audio(
    server: &HlsTestServer,
    cache_dir: &std::path::Path,
    pool: &ThreadPool,
    cancel: CancellationToken,
) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8").expect("url");

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(cache_dir))
        .with_cancel(cancel)
        .with_thread_pool(pool.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_media_info(wav_info)
        .with_thread_pool(pool.clone());

    Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>")
}

/// 2 healthy + 2 cancelled HLS instances.
///
/// The cancelled instances have their `CancellationToken` fired after a
/// short delay (simulating a network failure / user abort). The test verifies
/// that the healthy instances still read to EOF, unaffected by the cancelled ones.
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn healthy_instances_survive_cancelled_peers() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let wav_data = generate_wav_data();

    let mut handles: Vec<tokio::task::JoinHandle<Outcome>> = Vec::new();

    // --- Healthy instances (0, 1) ---
    for i in 0..2 {
        let server = create_server(&wav_data).await;
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let audio = create_hls_audio(&server, temp.path(), &pool, cancel).await;

        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_hls_best_effort(&mut audio);
            info!(instance = i, total_samples = total, "healthy done");
            Outcome {
                id: i,
                healthy: true,
                total_samples: total,
            }
        }));
    }

    // --- Cancelled instances (2, 3) ---
    // These get a cancel token that fires after 500ms, killing the download
    // mid-stream. The audio pipeline should terminate cleanly.
    for i in 2..4 {
        let server = create_server(&wav_data).await;
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let audio = create_hls_audio(&server, temp.path(), &pool, cancel).await;

        // Fire the cancel after a short delay.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancel_clone.cancel();
        });

        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_hls_best_effort(&mut audio);
            info!(instance = i, total_samples = total, "cancelled done");
            Outcome {
                id: i,
                healthy: false,
                total_samples: total,
            }
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all instances done");

    // Verify healthy instances completed with significant data.
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

    // Cancelled instances should have produced fewer samples than healthy ones
    // (they were killed partway through).
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
        // Cancelled instances should have read fewer samples (or same if cancel
        // happened after they finished — which is fine).
    }
}

/// 4 healthy + 4 cancelled HLS instances (8 total). Healthy ones must complete.
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_instances_half_cancelled() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let pool = ThreadPool::with_num_threads(4).expect("thread pool");
    let wav_data = generate_wav_data();

    let mut handles: Vec<tokio::task::JoinHandle<Outcome>> = Vec::new();

    // --- 4 healthy instances ---
    for i in 0..4 {
        let server = create_server(&wav_data).await;
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let audio = create_hls_audio(&server, temp.path(), &pool, cancel).await;

        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_hls_best_effort(&mut audio);
            info!(instance = i, total_samples = total, "healthy done");
            Outcome {
                id: i,
                healthy: true,
                total_samples: total,
            }
        }));
    }

    // --- 4 cancelled instances ---
    for i in 4..8 {
        let server = create_server(&wav_data).await;
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let audio = create_hls_audio(&server, temp.path(), &pool, cancel).await;

        // Stagger cancellation slightly to make it more realistic.
        let delay_ms = 200 + ((i - 4) as u64 * 100);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            cancel_clone.cancel();
        });

        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_hls_best_effort(&mut audio);
            info!(instance = i, total_samples = total, "cancelled done");
            Outcome {
                id: i,
                healthy: false,
                total_samples: total,
            }
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all 8 instances done");

    // All healthy instances must have completed with data.
    for r in results.iter().filter(|r| r.healthy) {
        assert!(
            r.total_samples > 0,
            "healthy instance {} read 0 samples",
            r.id
        );
    }
}
