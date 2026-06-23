use std::sync::Arc;

use kithara_assets::AssetScope;
use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_stream::dl::{Downloader, Peer, PeerHandle};

use crate::handle::VariantPeer;

/// Single owner of the raw transport + storage + headers quartet and the
/// sole `downloader.register` site. Vends one permanent narrow handle
/// ([`Self::variant_peer`]) plus a staged-transition distribution surface
/// ([`Self::peer_handle`] / [`Self::scope`] / [`Self::byte_pool`]) for the
/// loaders that still need full download + disk capability. Those three
/// accessors feed `PlaylistCache` / `KeyStore`'s existing params and are
/// retired at S13/S14 once those loaders narrow.
pub(crate) struct StreamPeer {
    peer_handle: PeerHandle,
    scope: AssetScope<DecryptContext>,
    byte_pool: kithara_bufpool::BytePool,
    headers: Option<kithara_net::Headers>,
}

impl StreamPeer {
    /// Register `peer` on `downloader` and take ownership of the quartet.
    /// The sole `downloader.register(...).with_bus(...)` site.
    pub(crate) fn register(
        downloader: &Downloader,
        peer: Arc<dyn Peer>,
        bus: EventBus,
        scope: AssetScope<DecryptContext>,
        byte_pool: kithara_bufpool::BytePool,
        headers: Option<kithara_net::Headers>,
    ) -> Self {
        let peer_handle = downloader.register(peer).with_bus(bus);
        Self {
            peer_handle,
            scope,
            byte_pool,
            headers,
        }
    }

    /// Permanent narrow vend: a [`VariantPeer`] carrying the shared peer
    /// and request headers. Live consumer today is `SizeEstimator`; the
    /// per-variant on-demand resolver picks it up at S13.
    pub(crate) fn variant_peer(&self) -> VariantPeer {
        VariantPeer::new(self.peer_handle.clone(), self.headers.clone())
    }

    /// Staged-transition distribution accessor (retired S13/S14).
    pub(crate) fn peer_handle(&self) -> PeerHandle {
        self.peer_handle.clone()
    }

    /// Staged-transition distribution accessor (retired S13/S14).
    pub(crate) fn scope(&self) -> AssetScope<DecryptContext> {
        self.scope.clone()
    }

    /// Staged-transition distribution accessor (retired S13/S14).
    pub(crate) fn byte_pool(&self) -> kithara_bufpool::BytePool {
        self.byte_pool.clone()
    }
}
