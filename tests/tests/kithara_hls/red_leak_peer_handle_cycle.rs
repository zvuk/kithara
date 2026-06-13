#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    sync::{Arc, atomic::AtomicBool},
    task::{Context, Poll},
};

use kithara_abr::Abr;
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{CancelToken, Mutex, time::Duration};
use kithara_stream::dl::{Downloader, DownloaderConfig, FetchCmd, Peer, PeerHandle};

/// Peer that stashes its own `PeerHandle` clone after registration —
/// mirroring `HlsPeer` + `SegmentLoader` in production.
struct SelfReferencingPeer {
    /// Hidden handle inside the peer's own state. The external caller
    /// has no knowledge of this clone.
    inner_handle: Mutex<Option<PeerHandle>>,
    _polled: AtomicBool,
}

impl SelfReferencingPeer {
    fn new() -> Self {
        Self {
            inner_handle: Mutex::new(None),
            _polled: AtomicBool::new(false),
        }
    }

    fn stash_handle(&self, handle: PeerHandle) {
        *self.inner_handle.lock() = Some(handle);
    }

    /// Mirror of `HlsPeer::teardown`: release the stashed `PeerHandle`
    /// clone so `PeerInner.cancel` can fire when the external handle
    /// drops, letting the Registry unregister this peer.
    fn teardown(&self) {
        *self.inner_handle.lock() = None;
    }
}

impl Abr for SelfReferencingPeer {}

impl Peer for SelfReferencingPeer {
    fn poll_next(&self, _cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        Poll::Pending
    }
}

/// A peer that stores its own `PeerHandle` forms a reference cycle with
/// the Registry: the stored clone keeps `PeerInner.cancel` unfired, so
/// the Registry never observes the handle drop and the peer arc leaks
/// until the whole Downloader shuts down.
///
/// The contract for self-referencing peers is therefore: *before
/// dropping the external handle, the user must ask the peer to release
/// its internal clones.* This is what `HlsSource::Drop →
/// HlsPeer::teardown()` does in production. This test exercises the
/// contract with a mini-peer.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn registry_releases_peer_when_teardown_clears_self_stored_handle()
-> Result<(), Box<dyn StdError + Send + Sync>> {
    let cancel = CancelToken::never();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .cancel(cancel.clone())
            .build(),
    );

    let peer: Arc<SelfReferencingPeer> = Arc::new(SelfReferencingPeer::new());
    let peer_dyn: Arc<dyn Peer> = peer.clone();
    let external_handle = downloader.register(peer_dyn);
    peer.stash_handle(external_handle.clone());

    peer.teardown();

    drop(external_handle);

    for _ in 0..40 {
        time::sleep(Duration::from_millis(50)).await;
        if Arc::strong_count(&peer) == 1 {
            break;
        }
    }

    let strong = Arc::strong_count(&peer);
    assert_eq!(
        strong, 1,
        "Registry still holds peer despite teardown + external handle drop: \
         strong_count={strong} (expected 1 — only the local test clone)."
    );

    cancel.cancel();
    Ok(())
}

/// Counter-proof: omitting `teardown()` leaves the cycle intact and the
/// Registry leaks the peer until the Downloader itself is cancelled.
#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn registry_leaks_peer_without_teardown_when_handle_is_self_stored()
-> Result<(), Box<dyn StdError + Send + Sync>> {
    let cancel = CancelToken::never();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .cancel(cancel.clone())
            .build(),
    );

    let peer: Arc<SelfReferencingPeer> = Arc::new(SelfReferencingPeer::new());
    let peer_dyn: Arc<dyn Peer> = peer.clone();
    let external_handle = downloader.register(peer_dyn);
    peer.stash_handle(external_handle.clone());

    drop(external_handle);

    for _ in 0..10 {
        time::sleep(Duration::from_millis(50)).await;
        if Arc::strong_count(&peer) == 1 {
            break;
        }
    }

    assert_eq!(
        Arc::strong_count(&peer),
        2,
        "Without teardown the Registry is expected to keep the peer arc alive \
         (cycle: Registry → Peer → stashed PeerHandle → PeerInner)."
    );

    peer.teardown();
    cancel.cancel();
    Ok(())
}
