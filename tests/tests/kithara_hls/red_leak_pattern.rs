#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    sync::{Arc, atomic::AtomicBool},
    task::{Context, Poll},
};

use kithara::{
    abr::Abr,
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        thread::active_named_thread_count,
        time::{self, Duration, Instant},
    },
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig, FetchCmd, Peer},
    },
};
use kithara_integration_tests::{TestTempDir, hls_server::TestServer, temp_dir};

/// Settle window / hang budget for [`wait_thread_count_quiesced`].
const SETTLE_WINDOW: usize = 4;
const SETTLE_BUDGET: Duration = Duration::from_secs(5);

/// Wait until kithara-owned thread teardown after a stream drop has quiesced —
/// i.e. `active_named_thread_count()` stops decreasing across a short settle
/// window. This is a state-wait (the loop condition reads the same named-thread
/// counter the leak assertion measures), not a timer pace: the inner
/// `time::sleep` is only the poll cadence and `budget` only bounds a hang.
///
/// The counter decrements when each `spawn_named` thread function RETURNS, so it
/// is observable once the engine is quiescent under the flash virtual clock —
/// unlike `ps -M` / `/proc/self/task`, which track OS reap that lags real time
/// and cannot be satisfied by virtual-clock advance.
async fn wait_thread_count_quiesced(settle_window: usize, budget: Duration) -> usize {
    const POLL: Duration = Duration::from_millis(25);
    let deadline = Instant::now() + budget;
    let mut last = active_named_thread_count();
    let mut stable = 0usize;
    loop {
        time::sleep(POLL).await;
        let now = active_named_thread_count();
        if now < last {
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
    let cancel = CancelToken::never();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .cancel(cancel.clone())
            .build(),
    );

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
        time::sleep(Duration::from_millis(50)).await;
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
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .cancel(CancelToken::never())
            .build(),
    );

    {
        let stream = Stream::<Hls>::new(
            HlsConfig::for_url(url.clone())
                .store(StoreOptions::new(temp_dir.path()))
                .cancel(CancelToken::never())
                .downloader(downloader.clone())
                .build(),
        )
        .await?;
        drop(stream);
    }

    // Wait until warmup teardown has quiesced (thread count stops dropping) —
    // state-wait on the same observable the leak assertion measures, not a timer.
    let threads_baseline = wait_thread_count_quiesced(SETTLE_WINDOW, SETTLE_BUDGET).await;

    for i in 0..ITERATIONS {
        let stream = Stream::<Hls>::new(
            HlsConfig::for_url(url.clone())
                .store(StoreOptions::new(temp_dir.path()))
                .cancel(CancelToken::never())
                .downloader(downloader.clone())
                .build(),
        )
        .await?;
        drop(stream);
        let threads = wait_thread_count_quiesced(SETTLE_WINDOW, SETTLE_BUDGET).await;
        tracing::info!(iter = i, threads, "post-drop");
    }

    let threads_after = wait_thread_count_quiesced(SETTLE_WINDOW, SETTLE_BUDGET).await;
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
