//! Concurrent File instance tests.
//!
//! Verifies that 2, 4, and 8 `Audio<Stream<File>>` instances can run
//! concurrently and each reads PCM data to EOF.

use std::path::Path;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_integration_tests::audio_fixture::AudioTestServer;
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread;
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::{TestTempDir, tracing_setup};
use tracing::info;

#[cfg(target_arch = "wasm32")]
const MAX_ZERO_READS: usize = 200;
#[cfg(target_arch = "wasm32")]
const MIN_SAMPLES_PER_INSTANCE: u64 = 8192;

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

#[cfg(target_arch = "wasm32")]
fn read_for_concurrency_check(audio: &mut Audio<Stream<File>>) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let mut zero_reads = 0usize;

    while total < MIN_SAMPLES_PER_INSTANCE && zero_reads < MAX_ZERO_READS {
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
        for &sample in &buf[..n] {
            assert!(sample.is_finite(), "non-finite sample at offset {total}");
        }
        total += n as u64;
    }

    assert!(
        total >= MIN_SAMPLES_PER_INSTANCE,
        "expected at least {MIN_SAMPLES_PER_INSTANCE} samples, got {total}",
    );
    total
}

#[cfg(not(target_arch = "wasm32"))]
fn read_for_concurrency_check(audio: &mut Audio<Stream<File>>) -> u64 {
    read_to_eof(audio)
}

/// Create an `Audio<Stream<File>>` for a remote MP3 URL.
async fn create_file_audio(url: url::Url, cache_dir: &Path) -> Audio<Stream<File>> {
    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(cache_dir));

    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");

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
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(120)))]
async fn two_file_instances(_tracing_setup: ()) {
    let server = AudioTestServer::new().await;

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..2 {
        let temp = TestTempDir::new();
        let audio = create_file_audio(server.mp3_url(), temp.path()).await;
        temps.push(temp);
        handles.push(spawn_blocking(move || {
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
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
    #[cfg(not(target_arch = "wasm32"))]
    assert_consistent_counts(&results);
}

/// 4 concurrent File instances on a shared pool.
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(120)))]
async fn four_file_instances(_tracing_setup: ()) {
    let server = AudioTestServer::new().await;

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..4 {
        let temp = TestTempDir::new();
        let audio = create_file_audio(server.mp3_url(), temp.path()).await;
        temps.push(temp);
        handles.push(spawn_blocking(move || {
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
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
    #[cfg(not(target_arch = "wasm32"))]
    assert_consistent_counts(&results);
}

/// 8 concurrent File instances on a shared pool.
#[kithara::test(tokio, browser, serial, timeout(Duration::from_secs(180)))]
async fn eight_file_instances(_tracing_setup: ()) {
    let server = AudioTestServer::new().await;

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..8 {
        let temp = TestTempDir::new();
        let audio = create_file_audio(server.mp3_url(), temp.path()).await;
        temps.push(temp);
        handles.push(spawn_blocking(move || {
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio);
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
    #[cfg(not(target_arch = "wasm32"))]
    assert_consistent_counts(&results);
}
