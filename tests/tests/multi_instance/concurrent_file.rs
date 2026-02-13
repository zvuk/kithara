//! Concurrent File instance tests.
//!
//! Verifies that 2, 4, and 8 `Audio<Stream<File>>` instances can run
//! concurrently on a shared `ThreadPool` and each reads PCM data to EOF.

use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig};
use kithara_file::{File, FileConfig};
use kithara_stream::{Stream, ThreadPool};
use rstest::rstest;
use tempfile::TempDir;
use tracing::info;

use crate::kithara_decode::fixture::AudioTestServer;

/// Read all PCM data from an `Audio` instance to EOF.
///
/// Returns the total number of samples read.
fn read_to_eof(audio: &mut Audio<Stream<File>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    loop {
        let n = audio.read(&mut buf);
        if n == 0 {
            break;
        }
        // Sanity: all samples must be finite
        for &s in &buf[..n] {
            assert!(s.is_finite(), "non-finite sample at offset {total}");
        }
        total += n as u64;
    }
    assert!(audio.is_eof(), "expected EOF after reading all data");
    total
}

/// Create an `Audio<Stream<File>>` for a remote MP3 URL.
async fn create_file_audio(
    url: url::Url,
    cache_dir: &std::path::Path,
    pool: &ThreadPool,
) -> Audio<Stream<File>> {
    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(cache_dir))
        .with_look_ahead_bytes(None)
        .with_thread_pool(pool.clone());

    let config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_thread_pool(pool.clone());

    Audio::<Stream<File>>::new(config)
        .await
        .expect("create Audio<Stream<File>>")
}

/// Assert that all instances produced a reasonable number of samples.
///
/// MP3 decoding can produce slightly different sample counts due to
/// encoder padding, so we check that they are within 1% of the mean.
fn assert_consistent_counts(results: &[(usize, u64)]) {
    let mean = results.iter().map(|(_, t)| *t).sum::<u64>() / results.len() as u64;
    let tolerance = mean / 100; // 1%
    for (id, total) in results {
        assert!(
            total.abs_diff(mean) <= tolerance,
            "instance {id} sample count {total} deviates >1% from mean {mean}"
        );
    }
}

// Tests

/// 2 concurrent File instances on a shared pool.
///
/// Each Audio instance uses 2 pool threads (downloader + audio_loop),
/// so pool size must be >= 2 * N to avoid starvation.
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_file_instances() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let server = AudioTestServer::new().await;
    let pool = ThreadPool::with_num_threads(6).expect("thread pool");

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..2 {
        let temp = TempDir::new().expect("temp dir");
        let audio = create_file_audio(server.mp3_url(), temp.path(), &pool).await;
        temps.push(temp);
        handles.push(tokio::task::spawn_blocking(move || {
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
    drop(temps);

    info!(?results, "all instances done");
    for (id, total) in &results {
        assert!(*total > 0, "instance {id} read 0 samples");
    }
    assert_consistent_counts(&results);
}

/// 4 concurrent File instances on a shared pool.
#[rstest]
#[timeout(Duration::from_secs(120))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_file_instances() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let server = AudioTestServer::new().await;
    let pool = ThreadPool::with_num_threads(10).expect("thread pool");

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..4 {
        let temp = TempDir::new().expect("temp dir");
        let audio = create_file_audio(server.mp3_url(), temp.path(), &pool).await;
        temps.push(temp);
        handles.push(tokio::task::spawn_blocking(move || {
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
    drop(temps);

    info!(?results, "all instances done");
    for (id, total) in &results {
        assert!(*total > 0, "instance {id} read 0 samples");
    }
    assert_consistent_counts(&results);
}

/// 8 concurrent File instances on a shared pool.
#[rstest]
#[timeout(Duration::from_secs(180))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_file_instances() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let server = AudioTestServer::new().await;
    let pool = ThreadPool::with_num_threads(18).expect("thread pool");

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..8 {
        let temp = TempDir::new().expect("temp dir");
        let audio = create_file_audio(server.mp3_url(), temp.path(), &pool).await;
        temps.push(temp);
        handles.push(tokio::task::spawn_blocking(move || {
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
    drop(temps);

    info!(?results, "all instances done");
    for (id, total) in &results {
        assert!(*total > 0, "instance {id} read 0 samples");
    }
    assert_consistent_counts(&results);
}
