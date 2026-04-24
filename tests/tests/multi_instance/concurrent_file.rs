//! Concurrent File instance tests.
//!
//! Verifies that N `Audio<Stream<File>>` instances can run concurrently
//! and each reads PCM data to EOF with roughly matching sample counts.

use std::path::Path;

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::{TestServerHelper, TestTempDir};
use tracing::info;

use crate::common::reader_helpers::{ReadLimit, read_for_concurrency_check};

struct Consts;
impl Consts {
    const READ_LIMIT: ReadLimit = ReadLimit::wasm_default();
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

async fn run_concurrent_file(n: usize) {
    let server = TestServerHelper::new().await;

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..n {
        let temp = TestTempDir::new();
        let audio = create_file_audio(server.asset("test.mp3"), temp.path()).await;
        temps.push(temp);
        handles.push(spawn_blocking(move || {
            let mut audio = audio;
            let total = read_for_concurrency_check(&mut audio, Consts::READ_LIMIT);
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

/// N concurrent File instances on a shared pool.
///
/// Each Audio instance uses 2 pool threads (downloader + `audio_loop`),
/// so pool size must be >= 2 * N to avoid starvation.
#[kithara::test(
    tokio,
    browser,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "2")
)]
#[case::n2(2)]
#[case::n4(4)]
#[case::n8(8)]
async fn concurrent_file_instances(#[case] instances: usize) {
    run_concurrent_file(instances).await;
}
