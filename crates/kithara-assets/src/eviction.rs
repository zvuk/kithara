
use std::sync::Arc;

use dashmap::DashMap;
use kithara_platform::tokio::sync::mpsc;

use crate::key::ResourceKey;

/// Per-`asset_root` eviction fanout: routes each evicted [`ResourceKey`] to the
/// single subscriber registered for its `asset_root` (last-writer-wins).
#[derive(Clone, Debug)]
pub struct EvictionRouter {
    subscribers: Arc<DashMap<Arc<str>, mpsc::UnboundedSender<ResourceKey>>>,
}

impl EvictionRouter {
    pub(crate) fn new() -> Self {
        Self {
            subscribers: Arc::new(DashMap::new()),
        }
    }

    /// Register `tx` to receive evictions under `asset_root`, returning an
    /// RAII guard that deregisters on drop. Last-writer-wins per root.
    pub(crate) fn subscribe(
        &self,
        asset_root: Arc<str>,
        tx: mpsc::UnboundedSender<ResourceKey>,
    ) -> EvictionSubscription {
        self.subscribers.insert(Arc::clone(&asset_root), tx);
        EvictionSubscription {
            subscribers: Arc::clone(&self.subscribers),
            asset_root,
        }
    }

    /// Route an evicted key to the subscriber that owns its `asset_root`.
    /// No-op for absolute keys (no `asset_root`) or unsubscribed roots.
    pub(crate) fn route(&self, key: &ResourceKey) {
        if let Some(root) = key.asset_root()
            && let Some(tx) = self.subscribers.get(root)
        {
            let _ = tx.send(key.clone());
        }
    }
}

/// RAII guard from
/// [`AssetStore::subscribe_eviction`](crate::AssetStore::subscribe_eviction);
/// deregisters the subscription on drop.
#[must_use = "drop the guard to deregister the eviction subscription"]
pub struct EvictionSubscription {
    subscribers: Arc<DashMap<Arc<str>, mpsc::UnboundedSender<ResourceKey>>>,
    asset_root: Arc<str>,
}

impl Drop for EvictionSubscription {
    fn drop(&mut self) {
        self.subscribers.remove(&self.asset_root);
    }
}
