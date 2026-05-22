#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{TestTempDir, hls_server::TestServer, temp_dir};
use kithara_platform::{
    time::{Duration, sleep},
    tokio::task::spawn_blocking,
};
use tokio_util::sync::CancellationToken;

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    const STREAM_ITERATIONS: usize = 4;
    const SEEKS_PER_STREAM: usize = 8;
    const PACKAGED_SEGMENT_SIZE: u64 = Shared::SEGMENT_SIZE as u64;
}

async fn build_small_cache_stream(
    server: &TestServer,
    temp_path: &std::path::Path,
    cancel: CancellationToken,
) -> Stream<Hls> {
    let url = server.url("/master.m3u8");
    let store = StoreOptions::builder()
        .cache_dir(temp_path.into())
        .is_ephemeral(true)
        .cache_capacity(NonZeroUsize::new(4).expect("nonzero"))
        .build();
    let config = HlsConfig::for_url(url)
        .store(store)
        .cancel(cancel)
        .initial_abr_mode(AbrMode::Manual(0))
        .build();
    Stream::<Hls>::new(config)
        .await
        .expect("HLS stream creation")
}

fn exercise_stream_blocking(mut stream: Stream<Hls>) {
    let mut buf = vec![0u8; 4096];
    let _ = stream.read(&mut buf[..64]);

    for i in 0..Consts::SEEKS_PER_STREAM {
        let seg = (i * 7) % 3;
        let within = ((i * 53) as u64) % Consts::PACKAGED_SEGMENT_SIZE;
        let pos = seg as u64 * Consts::PACKAGED_SEGMENT_SIZE + within;
        if stream.seek(SeekFrom::Start(pos)).is_err() {
            continue;
        }
        let _ = stream.read(&mut buf[..256]);
    }

    drop(stream);
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn red_small_cache_seek_stress_does_not_leak_threads(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;

    {
        let cancel = CancellationToken::new();
        let stream = build_small_cache_stream(&server, temp_dir.path(), cancel.clone()).await;
        spawn_blocking(move || exercise_stream_blocking(stream))
            .await
            .expect("warmup blocking join");
        cancel.cancel();
        sleep(Duration::from_millis(400)).await;
    }

    let threads_baseline = live_thread_count();

    for i in 0..Consts::STREAM_ITERATIONS {
        let cancel = CancellationToken::new();
        let stream = build_small_cache_stream(&server, temp_dir.path(), cancel.clone()).await;
        spawn_blocking(move || exercise_stream_blocking(stream))
            .await
            .expect("iteration blocking join");
        cancel.cancel();
        sleep(Duration::from_millis(150)).await;
        tracing::info!(iter = i, threads = live_thread_count(), "post-drop");
    }

    sleep(Duration::from_millis(300)).await;
    let threads_after = live_thread_count();
    let growth = threads_after.saturating_sub(threads_baseline);

    assert!(
        growth < 2,
        "OS thread count grew by {} over {} small-cache+seek iterations \
         (baseline={}, after={}). A growing thread count indicates that \
         per-stream background work (peer waker forwarder, decoder spawn, \
         eviction callback task, etc.) is not being reaped on stream drop — \
         this is the same class of leak nextest reports as LEAK on \
         live_ephemeral_small_cache_seek_stress_*.",
        growth,
        Consts::STREAM_ITERATIONS,
        threads_baseline,
        threads_after,
    );

    Ok(())
}

#[cfg(target_os = "macos")]
fn live_thread_count() -> usize {
    use std::process::Command;
    let out = Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps -M succeeded");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .count()
        .saturating_sub(1)
}

#[cfg(target_os = "linux")]
fn live_thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|it| it.count())
        .unwrap_or(0)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn live_thread_count() -> usize {
    0
}
