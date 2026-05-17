#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    sync::{Arc, atomic::AtomicBool},
    task::{Context, Poll},
};

use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_abr::Abr;
use kithara_integration_tests::hls_fixture::TestServer;
use kithara_platform::time::{Duration, sleep};
use kithara_stream::dl::{Downloader, DownloaderConfig, FetchCmd, Peer};
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio_util::sync::CancellationToken;

/// A peer that behaves like `HlsPeer`: `poll_next` never returns
/// `Ready(None)` — always `Pending`.
struct ImmortalPeer {
    _polled: AtomicBool,
}

impl ImmortalPeer {
    fn new() -> Self {
        Self {
            _polled: AtomicBool::new(false),
        }
    }
}

impl Abr for ImmortalPeer {}

impl Peer for ImmortalPeer {
    fn poll_next(&self, _cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        Poll::Pending
    }
}

/// Registry-level RED test.
///
/// After dropping the `PeerHandle`, the registered `Arc<dyn Peer>`
/// should eventually be released by the Downloader. If it isn't, the
/// strong count never drops to 1.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn red_registry_never_unregisters_pending_peer() -> Result<(), Box<dyn StdError + Send + Sync>>
{
    let cancel = CancellationToken::new();
    let downloader = Downloader::new(DownloaderConfig::builder().cancel(cancel.clone()).build());

    let peer: Arc<ImmortalPeer> = Arc::new(ImmortalPeer::new());
    let peer_dyn: Arc<dyn Peer> = peer.clone();
    let handle = downloader.register(peer_dyn.clone());
    drop(peer_dyn);

    assert!(
        Arc::strong_count(&peer) >= 2,
        "expected at least 2 strong refs (local + registry), got {}",
        Arc::strong_count(&peer)
    );

    drop(handle);

    for _ in 0..20 {
        sleep(Duration::from_millis(50)).await;
        if Arc::strong_count(&peer) == 1 {
            break;
        }
    }

    let final_refs = Arc::strong_count(&peer);
    assert_eq!(
        final_refs, 1,
        "Registry leaked peer: strong_count={} after dropping PeerHandle (expected 1 — only local clone)",
        final_refs
    );

    cancel.cancel();
    Ok(())
}

/// HLS-side RED test — exercises the same defect through a real
/// `Stream::<Hls>`. After the source is dropped, the `HlsPeer` should
/// be released. This test creates many streams against a single
/// shared Downloader and asserts that the OS thread count does not
/// grow proportionally.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn red_hls_source_drop_leaks_peer(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    const ITERATIONS: usize = 10;

    let server = TestServer::new().await;
    let url = server.url("/master.m3u8");

    let downloader = Downloader::new(
        DownloaderConfig::builder()
            .cancel(CancellationToken::new())
            .build(),
    );

    {
        let stream = Stream::<Hls>::new(
            HlsConfig::new(url.clone())
                .with_store(StoreOptions::new(temp_dir.path()))
                .with_cancel(CancellationToken::new())
                .with_downloader(downloader.clone()),
        )
        .await?;
        drop(stream);
        sleep(Duration::from_millis(300)).await;
    }

    let threads_baseline = live_thread_count();

    for i in 0..ITERATIONS {
        let stream = Stream::<Hls>::new(
            HlsConfig::new(url.clone())
                .with_store(StoreOptions::new(temp_dir.path()))
                .with_cancel(CancellationToken::new())
                .with_downloader(downloader.clone()),
        )
        .await?;
        drop(stream);
        sleep(Duration::from_millis(150)).await;
        tracing::info!(iter = i, threads = live_thread_count(), "post-drop");
    }

    sleep(Duration::from_millis(500)).await;
    let threads_after = live_thread_count();
    let growth = threads_after.saturating_sub(threads_baseline);

    assert!(
        growth < 2,
        "OS thread count grew by {} over {} iterations (baseline={}, after={}). \
         Growth indicates peers/tasks aren't being cleaned up on HlsSource drop.",
        growth,
        ITERATIONS,
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
