//! RED test: systemic leak in `Downloader::Registry` peer lifecycle.
//!
//! Hypothesis
//! In `registry.rs::poll_peers`, a registered peer is only removed from
//! `self.peers` when BOTH:
//!
//!   - `entry.peer_done == true` — set when `Peer::poll_next` returned
//!     `Poll::Ready(None)`; AND
//!   - `entry.peer_cancel.is_cancelled() == true` — `peer_cancel` is a
//!     *child* of the cancel token passed to `Registry::add`.
//!
//! On the HLS side:
//!   1. `HlsPeer::poll_next` never returns `Ready(None)`; it only returns
//!      `Pending` or `Ready(Some(_))`. So `peer_done` stays `false`.
//!   2. `HlsSource` only holds a `PeerHandle` (`_peer_handle`). Dropping
//!      `HlsSource` drops the `PeerHandle`, which fires `PeerInner::cancel`
//!      (a cancel token). But that cancel token is **not the same token**
//!      as the one Registry's `peer_cancel` is a child of. Registry's
//!      `peer_cancel` is a child of `inner.cancel` (the whole-Downloader
//!      cancel), not of the PeerHandle's cancel.
//!
//! → Registry never removes the `Arc<dyn Peer>` entry for this source.
//! → The `Arc<HlsPeer>` lives forever (until the Downloader itself is
//!   dropped).
//! → The waker-forwarding task spawned in `HlsPeer::activate` also
//!   never terminates, because it holds `Arc::clone(self)` AND its
//!   `wake_cancel` + `scheduler.coord.cancel` are never fired by a
//!   plain source drop.
//!
//! This test proves the Registry half of the leak (the part that is
//! **not HLS-specific**). It uses a custom `Peer` whose `poll_next`
//! never returns `None`, registers it, drops the `PeerHandle`, and
//! asserts the peer's `Arc` count drops to 1 within a reasonable
//! window. It FAILS today because the Registry keeps its clone alive.
//!
//! If you want the HLS-side observation instead, see the second test
//! which exercises the same defect through a real `Stream::<Hls>`.

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

impl Peer for ImmortalPeer {
    fn poll_next(&self, _cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        // Never finish: just like HlsPeer. We deliberately do not
        // register the waker here — poll_peers will keep checking us
        // on every tick because other peers / registrations may wake
        // the poll_fn, which is exactly what happens in HLS.
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
    let downloader = Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()));

    let peer: Arc<ImmortalPeer> = Arc::new(ImmortalPeer::new());
    let peer_dyn: Arc<dyn Peer> = peer.clone();
    let handle = downloader.register(peer_dyn.clone());
    drop(peer_dyn);

    // Baseline: our local `peer` Arc + the one stored in Registry = 2.
    assert!(
        Arc::strong_count(&peer) >= 2,
        "expected at least 2 strong refs (local + registry), got {}",
        Arc::strong_count(&peer)
    );

    // Drop the handle. This fires `PeerInner::cancel` (a cancel token
    // separate from Registry's peer_cancel). The Registry's poll_peers
    // never observes this cancel, so the peer entry is not removed.
    drop(handle);

    // Give the Downloader loop plenty of time to unregister.
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

    // Clean up by cancelling the whole Downloader.
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

    let downloader =
        Downloader::new(DownloaderConfig::default().with_cancel(CancellationToken::new()));

    // Warm up so the baseline excludes first-time tokio worker growth.
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

    // If each HlsPeer's waker-forwarding task + registry entry linger,
    // we leak per iteration. We allow <2 threads of drift as normal.
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
