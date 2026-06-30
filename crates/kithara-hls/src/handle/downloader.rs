use std::sync::Arc;

use kithara_assets::AssetScope;
use kithara_events::EventBus;
use kithara_stream::dl::{Downloader, Peer, PeerHandle};

/// Single owner of the raw transport + storage + headers quartet and the
/// sole `downloader.register` site. Vends one permanent narrow handle
/// ([`Self::peer_handle`] / [`Self::scope`] / [`Self::byte_pool`]) for the
/// loaders that still need full download + disk capability.
pub(crate) struct StreamPeer {
    peer_handle: PeerHandle,
    scope: AssetScope,
    byte_pool: kithara_bufpool::BytePool,
}

impl StreamPeer {
    /// Register `peer` on `downloader` and take ownership of the quartet.
    /// The sole `downloader.register(...).with_bus(...)` site.
    pub(crate) fn register(
        downloader: &Downloader,
        peer: Arc<dyn Peer>,
        bus: EventBus,
        scope: AssetScope,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Self {
        let peer_handle = downloader.register(peer).with_bus(bus);
        Self {
            peer_handle,
            scope,
            byte_pool,
        }
    }

    /// Staged-transition distribution accessor (retired S13/S14).
    pub(crate) fn peer_handle(&self) -> PeerHandle {
        self.peer_handle.clone()
    }

    /// Staged-transition distribution accessor (retired S13/S14).
    pub(crate) fn scope(&self) -> AssetScope {
        self.scope.clone()
    }

    /// Staged-transition distribution accessor (retired S13/S14).
    pub(crate) fn byte_pool(&self) -> kithara_bufpool::BytePool {
        self.byte_pool.clone()
    }
}
