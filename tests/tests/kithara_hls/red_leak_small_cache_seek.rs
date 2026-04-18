//! RED test: nextest LEAK on `live_ephemeral_small_cache_seek_stress_*`.
//!
//! Hypothesis
//! On the `*_hw`/`*_sw` variants the test body returns quickly but the
//! process is marked `LEAK [~0.6s]` by nextest, meaning the test process
//! still has live tasks / threads past nextest's leak-timeout (~100 ms).
//!
//! The two cases in the original report share the `with_ephemeral(true)`
//! + `with_cache_capacity(NonZeroUsize::new(4))` + seek-stress path but
//! differ in decoder (HW vs SW/DRM). A common upstream cause in the
//! asset/stream layer is therefore more plausible than decoder-specific
//! leaks.
//!
//! Candidates, in order of likelihood:
//!
//! 1. `CachedAssets::cache_entry` evicts via `on_invalidated` callback
//!    (see `crates/kithara-assets/src/cache.rs:286`), which in turn runs
//!    `AvailabilityIndex::remove` (see `store.rs:435`) and the user's
//!    callback. If eviction fires while a writer / reader still holds
//!    a strong `Arc` to the evicted `ProcessedResource`, that resource
//!    outlives the LRU entry. On stream drop the resource handle may
//!    keep a `CancellationToken` child, a waker forwarder, or a
//!    decoder-side `tokio::spawn` alive past the top-level cancel.
//!
//! 2. The `HlsPeer` waker-forwarding task documented in
//!    `red_leak_pattern.rs` is the best-known leak in this tree; the
//!    fix landed but the seek-stress variant may still hit a different
//!    variant-switch race where `filling_layout_gap` is left true on
//!    the dropped peer.
//!
//! 3. On every seek under a 4-slot LRU, a fresh `acquire_resource_with_ctx`
//!    races with the previous segment's writer. `ProcessedResource::reactivate`
//!    was recently fixed (38d51adfb) to clear the `processed` flag, but
//!    seek-stress exercises many reactivate cycles per test — if any
//!    spawn occurs inside that path and isn't joined, it lingers.
//!
//! Strategy of this RED
//! This file contains a native-only, packaged-fixture leak probe:
//!
//! * start `TestServer`, build `Stream<Hls>` with the *same* store
//!   options as the stress test (ephemeral + 4-slot LRU);
//! * do a warmup read, issue N seeks that force LRU churn, then drop
//!   the stream and all handles;
//! * snapshot OS thread count before stream creation (baseline) and
//!   again ~300 ms after drop;
//! * assert the growth is < 2 threads.
//!
//! This mirrors the nextest LEAK-detection semantics but is observable
//! inside a single test process.

#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::hls_fixture::TestServer;
use kithara_platform::{
    time::{Duration, sleep},
    tokio::task::spawn_blocking,
};
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio_util::sync::CancellationToken;

struct Consts;
impl Consts {
    const STREAM_ITERATIONS: usize = 4;
    const SEEKS_PER_STREAM: usize = 8;
    const PACKAGED_SEGMENT_SIZE: u64 = 200_000;
}

async fn build_small_cache_stream(
    server: &TestServer,
    temp_path: &std::path::Path,
    cancel: CancellationToken,
) -> Stream<Hls> {
    let url = server.url("/master.m3u8");
    let store = StoreOptions::new(temp_path)
        .with_ephemeral(true)
        .with_cache_capacity(NonZeroUsize::new(4).expect("nonzero"));
    let config = HlsConfig::new(url)
        .with_store(store)
        .with_cancel(cancel)
        .with_abr_options(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });
    Stream::<Hls>::new(config)
        .await
        .expect("HLS stream creation")
}

fn exercise_stream_blocking(mut stream: Stream<Hls>) {
    let mut buf = vec![0u8; 4096];
    let _ = stream.read(&mut buf[..64]);

    for i in 0..Consts::SEEKS_PER_STREAM {
        // Pseudo-random positions across 3 segments (each 200_000 bytes).
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

    // Warm up tokio worker pool + first-time allocations so they do
    // not pollute the baseline.
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
        // Drop the cancel-token owner last, matching production where
        // user code typically owns `cancel` outside the stream.
        cancel.cancel();
        sleep(Duration::from_millis(150)).await;
        tracing::info!(iter = i, threads = live_thread_count(), "post-drop");
    }

    // Mirror nextest leak-timeout: give the process up to ~300 ms
    // to reap background tasks.
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
