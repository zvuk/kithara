//! Regression: the `Registry → Arc<dyn Peer> → PeerHandle` reference
//! cycle and the tear-down contract that breaks it.
//!
//! Background
//! `HlsPeer` is held by the `Downloader` Registry as `Arc<dyn Peer>`. The
//! Registry only drops that Arc when `entry.peer_cancel.is_cancelled()`
//! becomes true. `peer_cancel` is the `PeerHandle`'s inner cancel token,
//! which fires from `PeerInner::Drop` when the last `PeerHandle` clone is
//! dropped.
//!
//! On activation, `HlsPeer` stores an `Arc<SegmentLoader>` inside its
//! internal `HlsState`. The `SegmentLoader` + `PlaylistCache` +
//! `KeyManager` each hold a `PeerHandle` clone.
//!
//! Cycle: `Registry → Arc<HlsPeer> → HlsState.loader → PeerHandle →
//! PeerInner (cancel unfired) → Registry keeps peer`.
//!
//! Contract
//! Breaking the cycle requires *the peer* to release its internal
//! `PeerHandle` clones when the user-visible source drops. In production
//! this is done by `HlsSource::Drop` calling `HlsPeer::teardown()`, which
//! clears the stashed `HlsState`. This test documents the contract by
//! exercising a mini-peer with an explicit `teardown()` method that
//! mirrors `HlsPeer::teardown`. Without `teardown`, the Registry leaks
//! the peer until the `Downloader` itself is dropped.

#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    sync::{Arc, atomic::AtomicBool},
    task::{Context, Poll},
};

use kithara_platform::{
    Mutex,
    time::{Duration, sleep},
};
use kithara_stream::dl::{Downloader, DownloaderConfig, FetchCmd, Peer, PeerHandle};
use tokio_util::sync::CancellationToken;

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
        *self.inner_handle.lock_sync() = Some(handle);
    }

    /// Mirror of `HlsPeer::teardown`: release the stashed PeerHandle
    /// clone so `PeerInner.cancel` can fire when the external handle
    /// drops, letting the Registry unregister this peer.
    fn teardown(&self) {
        *self.inner_handle.lock_sync() = None;
    }
}

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
    let cancel = CancellationToken::new();
    let downloader = Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()));

    // 1. Register and stash an internal PeerHandle clone inside the
    //    peer itself — mirroring `HlsPeer::activate()` passing
    //    `Arc::clone(&loader)` which transitively holds the PeerHandle.
    let peer: Arc<SelfReferencingPeer> = Arc::new(SelfReferencingPeer::new());
    let peer_dyn: Arc<dyn Peer> = peer.clone();
    let external_handle = downloader.register(peer_dyn);
    peer.stash_handle(external_handle.clone());

    // 2. Sanity: the stashed clone alone is enough to keep the peer's
    //    `PeerInner` alive — even without an explicit `external_handle`
    //    still outstanding — until `teardown()` releases it.
    //
    //    Simulate production: the user's `HlsSource::Drop` calls
    //    `HlsPeer::teardown()` *before* the external handle is dropped.
    peer.teardown();

    // 3. Now drop the external handle. Only at this point does
    //    `PeerInner` lose its final strong reference, fires cancel,
    //    and the Registry releases the peer arc on its next poll.
    drop(external_handle);

    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
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

    // Clean up.
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
    let cancel = CancellationToken::new();
    let downloader = Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()));

    let peer: Arc<SelfReferencingPeer> = Arc::new(SelfReferencingPeer::new());
    let peer_dyn: Arc<dyn Peer> = peer.clone();
    let external_handle = downloader.register(peer_dyn);
    peer.stash_handle(external_handle.clone());

    drop(external_handle);

    // Wait a reasonable window. The cycle cannot resolve without teardown.
    for _ in 0..10 {
        sleep(Duration::from_millis(50)).await;
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

    // Explicit teardown + cancel to avoid leaking into the Downloader's
    // shutdown path on test completion.
    peer.teardown();
    cancel.cancel();
    Ok(())
}
