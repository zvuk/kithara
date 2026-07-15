use dashmap::DashMap;
use kithara_platform::{sync::Arc, tokio::sync::mpsc};

use crate::layout::ResourceKey;

/// Per-`asset_root` eviction fanout: routes each evicted [`ResourceKey`] to the
/// single subscriber registered for its `asset_root` (last-writer-wins).
#[derive(Clone, Debug, Default)]
pub struct EvictionRouter {
    subscribers: Arc<DashMap<Arc<str>, mpsc::UnboundedSender<ResourceKey>>>,
}

impl EvictionRouter {
    /// Route an evicted key to the subscriber that owns its `asset_root`.
    /// No-op for absolute keys (no `asset_root`) or unsubscribed roots.
    pub(crate) fn route(&self, key: &ResourceKey) {
        if let Some(root) = key.asset_root()
            && let Some(tx) = self.subscribers.get(root)
        {
            let _ = tx.send(key.clone());
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
            asset_root,
            subscribers: Arc::clone(&self.subscribers),
        }
    }
}

/// RAII guard from
/// [`AssetStore::subscribe_eviction`](crate::AssetStore::subscribe_eviction);
/// deregisters the subscription on drop.
#[must_use = "drop the guard to deregister the eviction subscription"]
pub struct EvictionSubscription {
    asset_root: Arc<str>,
    subscribers: Arc<DashMap<Arc<str>, mpsc::UnboundedSender<ResourceKey>>>,
}

impl Drop for EvictionSubscription {
    fn drop(&mut self) {
        self.subscribers.remove(&self.asset_root);
    }
}
