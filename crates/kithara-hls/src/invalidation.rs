#![forbid(unsafe_code)]

//! Shared HLS asset store and its invalidation routing.
//!
//! [`HlsStore`] bundles a shared `AssetStore<DecryptContext>` with a
//! private registry that routes per-`asset_root` cache invalidations to
//! the matching live stream's eviction channel. Inject one store into
//! every HLS resource that should cooperate on a single cache; the
//! registry is wired automatically by
//! [`build_shared_asset_store`](crate::build_shared_asset_store) and
//! never surfaces in any config. See `README.md` "Shared Asset Store".

use std::sync::Arc;

use dashmap::DashMap;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::tokio::sync::mpsc;

/// Registry keyed by `asset_root`, private to the owning [`HlsStore`].
/// Maps each live stream's `asset_root` to the eviction channel its
/// `HlsPeer` drains, so a single shared `on_invalidated` callback can
/// route an evicted key back to the stream that owns it.
pub(crate) type HlsInvalidationRegistry =
    Arc<DashMap<Arc<str>, mpsc::UnboundedSender<ResourceKey>>>;

/// Shared HLS asset store: the backend plus its private invalidation
/// registry. Construct it via
/// [`build_shared_asset_store`](crate::build_shared_asset_store); the
/// registry inside is wired automatically and stays opaque to callers.
#[derive(Clone)]
#[non_exhaustive]
pub struct HlsStore {
    pub(crate) backend: Arc<AssetStore<DecryptContext>>,
    pub(crate) registry: HlsInvalidationRegistry,
}

/// RAII guard owned by `HlsSource`. Drops the matching registry entry
/// when the source dies so future invalidations no-op for the gone
/// stream.
#[must_use = "drop the guard to deregister from the invalidation registry"]
pub(crate) struct HlsInvalidationGuard {
    registry: HlsInvalidationRegistry,
    asset_root: Arc<str>,
}

impl HlsInvalidationGuard {
    pub(crate) fn install(
        registry: HlsInvalidationRegistry,
        asset_root: Arc<str>,
        evict_tx: mpsc::UnboundedSender<ResourceKey>,
    ) -> Self {
        registry.insert(Arc::clone(&asset_root), evict_tx);
        Self {
            registry,
            asset_root,
        }
    }
}

impl Drop for HlsInvalidationGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.asset_root);
    }
}
