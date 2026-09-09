use std::path::Path;

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::AudioConfig,
    file::{File, FileConfig},
    platform::{time::Duration, tokio::task::spawn_blocking},
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir,
    bufpool_ext::{TestPools, pools},
    reads::{ReadLimit, read_for_concurrency_check},
};
use kithara_test_fixtures::SignalAsset;
use tracing::info;
use url::Url;

/// Create an `Audio<Stream<File>>` for a remote MP3 URL.
async fn create_file_audio(
    url: Url,
    cache_dir: &Path,
) -> RegisteredAudio<Stream<File<TestPools>>, TestPools> {
    let pools = pools();
    let file_config = FileConfig::for_src(url.into())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: cache_dir.into(),
                })
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = AudioConfig::<File<TestPools>>::for_stream(file_config)
        .hint(("mp3").to_string())
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    worker
        .open(config)
        .await
        .expect("create Audio<Stream<File>>")
}

/// Assert that all instances produced a reasonable number of samples.
///
/// MP3 decoding can produce slightly different sample counts due to
/// encoder padding, so we check that they are within 1% of the mean.
fn assert_consistent_counts(results: &[(usize, u64)]) {
    let mean = results.iter().map(|(_, t)| *t).sum::<u64>() / results.len() as u64;
    let tolerance = mean / 100;
    for (id, total) in results {
        assert!(
            total.abs_diff(mean) <= tolerance,
            "instance {id} sample count {total} deviates >1% from mean {mean}"
        );
    }
}

async fn run_concurrent_file(n: usize, source: (TestServerHelper, Url)) {
    let (_server, url) = source;

    let mut handles = Vec::new();
    let mut temps = Vec::new();
    for i in 0..n {
        let temp = TestTempDir::new();
        let audio = create_file_audio(url.clone(), temp.path()).await;
        temps.push(temp);
        handles.push(spawn_blocking(move || {
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
    hang_timeout_secs(2)
)]
#[case::n2(2)]
#[case::n4(4)]
#[case::n8(8)]
async fn concurrent_file_instances(
    #[case] instances: usize,
    #[future(awt)] file_source: (TestServerHelper, Url),
) {
    run_concurrent_file(instances, file_source).await;
}

#[kithara::fixture]
async fn file_source() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::MP3_TRACK_SINE440_187S);
    (server, url)
}
