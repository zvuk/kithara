#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        CancelToken,
        thread::active_named_thread_count,
        time::{self, Duration, Instant},
        tokio::task::spawn_blocking,
    },
    stream::Stream,
};
use kithara_integration_tests::{TestTempDir, hls_server::TestServer, temp_dir};

use crate::common::test_defaults::Consts as Shared;

/// Wait until kithara's per-stream background work has quiesced after a stream
/// drop+cancel — i.e. `active_named_thread_count()` stops decreasing across a
/// short settle window. This is a state-wait, not a timer pace: the loop
/// condition checks the runtime-internal named-thread counter, which is
/// decremented when each spawned thread function returns and so flips under the
/// flash clock (unlike `live_thread_count()`'s `ps -M` / `/proc/self/task`
/// read, which tracks OS reap that lags real time and cannot be satisfied by
/// virtual-clock advance alone). The inner `time::sleep` is only the poll
/// cadence (mirrors `settle_thread_count` in the sibling DRM leak test). The
/// `budget` bounds a hang via a real wall deadline; it does not pace progress.
async fn wait_thread_count_quiesced(settle_window: usize, budget: Duration) -> usize {
    const POLL: Duration = Duration::from_millis(25);
    let deadline = Instant::now() + budget;
    let mut last = active_named_thread_count();
    let mut stable = 0usize;
    loop {
        time::sleep(POLL).await;
        let now = active_named_thread_count();
        if now < last {
            // teardown still in progress — named-thread count is still dropping
            stable = 0;
        } else {
            stable += 1;
        }
        last = now;
        if stable >= settle_window || Instant::now() >= deadline {
            return now;
        }
    }
}

struct Consts;
impl Consts {
    const STREAM_ITERATIONS: usize = 4;
    const SEEKS_PER_STREAM: usize = 8;
    const PACKAGED_SEGMENT_SIZE: u64 = Shared::SEGMENT_SIZE as u64;
    const WARMUP_MAX_STREAMS: usize = 8;
    const WARMUP_STABLE_SAMPLES: usize = 2;
}

async fn build_small_cache_stream(
    server: &TestServer,
    temp_path: &std::path::Path,
    cancel: CancelToken,
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
        .initial_abr_mode(AbrMode::manual(0))
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

async fn run_small_cache_seek_cycle(server: &TestServer, temp_path: &std::path::Path) -> usize {
    let cancel = CancelToken::never();
    let stream = build_small_cache_stream(server, temp_path, cancel.clone()).await;
    spawn_blocking(move || exercise_stream_blocking(stream))
        .await
        .expect("blocking join");
    cancel.cancel();
    wait_thread_count_quiesced(4, Duration::from_secs(5)).await
}

async fn stable_live_thread_baseline(server: &TestServer, temp_path: &std::path::Path) -> usize {
    let mut last = None;
    let mut stable = 0usize;

    for _ in 0..Consts::WARMUP_MAX_STREAMS {
        run_small_cache_seek_cycle(server, temp_path).await;
        let now = live_thread_count();
        if Some(now) == last {
            stable += 1;
        } else {
            stable = 0;
        }
        last = Some(now);
        if stable >= Consts::WARMUP_STABLE_SAMPLES {
            return now;
        }
    }

    last.unwrap_or_else(live_thread_count)
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

    let threads_baseline = stable_live_thread_baseline(&server, temp_dir.path()).await;

    for i in 0..Consts::STREAM_ITERATIONS {
        // Wait until this iteration's per-stream tasks are reaped (thread count
        // stops dropping) before logging — not a fixed pacing delay.
        let threads = run_small_cache_seek_cycle(&server, temp_dir.path()).await;
        tracing::info!(iter = i, threads, "post-drop");
    }

    // Final settle: wait until reaping has fully quiesced before the leak
    // assertion samples the thread count.
    wait_thread_count_quiesced(6, Duration::from_secs(5)).await;
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
