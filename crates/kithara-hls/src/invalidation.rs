#![forbid(unsafe_code)]

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
    asset_root: Arc<str>,
    registry: HlsInvalidationRegistry,
}

impl HlsInvalidationGuard {
    pub(crate) fn install(
        registry: HlsInvalidationRegistry,
        asset_root: Arc<str>,
        evict_tx: mpsc::UnboundedSender<ResourceKey>,
    ) -> Self {
        registry.insert(Arc::clone(&asset_root), evict_tx);
        Self {
            asset_root,
            registry,
        }
    }
}

impl Drop for HlsInvalidationGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.asset_root);
    }
}
