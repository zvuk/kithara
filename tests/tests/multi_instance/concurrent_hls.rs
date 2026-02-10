//! Concurrent HLS instance tests.
//!
//! Verifies that 2, 4, and 8 `Audio<Stream<Hls>>` instances can run
//! concurrently on a shared `ThreadPool` and each reads PCM data to EOF.
//! Tests both manual variant (no ABR) and auto ABR modes.

use std::{sync::Arc, time::Duration};

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, ThreadPool};
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::wav::create_test_wav;
use crate::kithara_hls::fixture::{HlsTestServer, HlsTestServerConfig};

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 10; // Smaller than stress test — enough for concurrency check.

/// Read all PCM data from an `Audio<Stream<Hls>>` instance to EOF.
///
/// Returns the total number of samples read.
fn read_to_eof(audio: &mut Audio<Stream<Hls>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    loop {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        for &s in &buf[..n] {
            assert!(s.is_finite(), "non-finite sample at offset {total}");
        }
        total += n as u64;
    }
    assert!(audio.is_eof(), "expected EOF after reading all data");
    total
}

/// Create an HLS server with WAV segments.
async fn create_hls_server(wav_data: Arc<Vec<u8>>) -> HlsTestServer {
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(wav_data),
        ..Default::default()
    })
    .await
}

/// Create an HLS server with 2 ABR variants of different bandwidth.
async fn create_hls_server_abr(wav_data: Arc<Vec<u8>>) -> HlsTestServer {
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(wav_data),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        ..Default::default()
    })
    .await
}

/// Create an `Audio<Stream<Hls>>` for a single-variant (manual ABR) stream.
async fn create_hls_audio(
    server: &HlsTestServer,
    cache_dir: &std::path::Path,
    pool: &ThreadPool,
) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8").expect("url");
    let cancel = CancellationToken::new();

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

/// Create an `Audio<Stream<Hls>>` with auto ABR.
async fn create_hls_audio_abr(
    server: &HlsTestServer,
    cache_dir: &std::path::Path,
    pool: &ThreadPool,
) -> Audio<Stream<Hls>> {
    let url = server.url("/master.m3u8").expect("url");
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(cache_dir))
        .with_cancel(cancel)
        .with_thread_pool(pool.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));

    let config = AudioConfig::<Hls>::new(hls_config)
        .with_media_info(wav_info)
        .with_thread_pool(pool.clone());

    Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> with ABR")
}

/// Generate WAV data for the test (total size = segments * segment_size).
fn generate_wav_data() -> Arc<Vec<u8>> {
    let total_bytes = SEGMENT_COUNT * SEGMENT_SIZE;
    let bytes_per_frame = CHANNELS as usize * 2;
    let header_size = 44;
    let sample_count = (total_bytes - header_size) / bytes_per_frame;
    Arc::new(create_test_wav(sample_count, SAMPLE_RATE, CHANNELS))
}

// Tests

/// 2 concurrent HLS instances (manual variant, no ABR).
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hls_instances() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let wav_data = generate_wav_data();
    let pool = ThreadPool::with_num_threads(4).expect("thread pool");

    // Each instance needs its own server (binds a random port) and cache dir.
    let mut handles = Vec::new();
    for i in 0..2 {
        let server = create_hls_server(Arc::clone(&wav_data)).await;
        let temp = TempDir::new().expect("temp dir");
        let audio = create_hls_audio(&server, temp.path(), &pool).await;
        // Keep server and temp alive until reader finishes.
        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_to_eof(&mut audio);
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

/// 4 concurrent HLS instances (manual variant, no ABR).
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_hls_instances() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let wav_data = generate_wav_data();
    let pool = ThreadPool::with_num_threads(4).expect("thread pool");

    let mut handles = Vec::new();
    for i in 0..4 {
        let server = create_hls_server(Arc::clone(&wav_data)).await;
        let temp = TempDir::new().expect("temp dir");
        let audio = create_hls_audio(&server, temp.path(), &pool).await;
        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_to_eof(&mut audio);
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

/// 8 concurrent HLS instances (manual variant, no ABR).
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_hls_instances() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let wav_data = generate_wav_data();
    let pool = ThreadPool::with_num_threads(4).expect("thread pool");

    let mut handles = Vec::new();
    for i in 0..8 {
        let server = create_hls_server(Arc::clone(&wav_data)).await;
        let temp = TempDir::new().expect("temp dir");
        let audio = create_hls_audio(&server, temp.path(), &pool).await;
        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_to_eof(&mut audio);
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

/// 4 concurrent HLS instances with auto ABR (2 variants).
#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_hls_instances_with_abr() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let wav_data = generate_wav_data();
    let pool = ThreadPool::with_num_threads(4).expect("thread pool");

    let mut handles = Vec::new();
    for i in 0..4 {
        let server = create_hls_server_abr(Arc::clone(&wav_data)).await;
        let temp = TempDir::new().expect("temp dir");
        let audio = create_hls_audio_abr(&server, temp.path(), &pool).await;
        handles.push(tokio::task::spawn_blocking(move || {
            let _server = server;
            let _temp = temp;
            let mut audio = audio;
            let total = read_to_eof(&mut audio);
            info!(instance = i, total_samples = total, "instance finished");
            (i, total)
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.expect("join"));
    }

    info!(?results, "all HLS+ABR instances done");
    for (id, total) in &results {
        assert!(*total > 0, "HLS+ABR instance {id} read 0 samples");
    }
}
