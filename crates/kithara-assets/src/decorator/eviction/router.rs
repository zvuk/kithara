use std::{
    collections::{HashMap, hash_map::Entry},
    fmt,
};

use kithara_platform::{
    sync::{Arc, Mutex},
    tokio::sync::mpsc,
};

use crate::layout::ResourceKey;

type SubscriptionId = u64;
type RootSubscribers = HashMap<SubscriptionId, mpsc::UnboundedSender<ResourceKey>>;

#[derive(Default)]
struct RouterState {
    next_id: SubscriptionId,
    subscribers: HashMap<Arc<str>, RootSubscribers>,
}

impl RouterState {
    fn allocate_id(&mut self) -> SubscriptionId {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if self
                .subscribers
                .values()
                .all(|subscribers| !subscribers.contains_key(&id))
            {
                return id;
            }
        }
    }

    fn remove(&mut self, asset_root: &Arc<str>, id: SubscriptionId) {
        let Entry::Occupied(mut subscribers) = self.subscribers.entry(Arc::clone(asset_root))
        else {
            return;
        };
        subscribers.get_mut().remove(&id);
        if subscribers.get().is_empty() {
            subscribers.remove();
        }
    }
}

/// Per-`asset_root` eviction fanout.
#[derive(Clone, Default)]
pub(crate) struct EvictionRouter {
    state: Arc<Mutex<RouterState>>,
}

impl fmt::Debug for EvictionRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvictionRouter").finish_non_exhaustive()
    }
}

impl EvictionRouter {
    /// Route an evicted key to every subscriber for its `asset_root`.
    /// No-op for absolute keys (no `asset_root`) or unsubscribed roots.
    pub(crate) fn route(&self, key: &ResourceKey) {
        let Some(root) = key.asset_root() else {
            return;
        };
        let subscribers = self
            .state
            .lock()
            .subscribers
            .get(root)
            .map(|subscribers| subscribers.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for tx in subscribers {
            let _ = tx.send(key.clone());
        }
    }

    /// Register `tx` to receive evictions under `asset_root`, returning an
    /// RAII guard that deregisters only this subscription on drop.
    pub(crate) fn subscribe(
        &self,
        asset_root: Arc<str>,
        tx: mpsc::UnboundedSender<ResourceKey>,
    ) -> EvictionSubscription {
        let id = {
            let mut state = self.state.lock();
            let id = state.allocate_id();
            state
                .subscribers
                .entry(Arc::clone(&asset_root))
                .or_default()
                .insert(id, tx);
            id
        };
        EvictionSubscription {
            asset_root,
            id,
            state: Arc::clone(&self.state),
        }
    }
}

/// RAII guard from
/// [`AssetStore::subscribe_eviction`](crate::AssetStore::subscribe_eviction);
/// deregisters the subscription on drop.
#[must_use = "drop the guard to deregister the eviction subscription"]
pub struct EvictionSubscription {
    asset_root: Arc<str>,
    id: SubscriptionId,
    state: Arc<Mutex<RouterState>>,
}

impl Drop for EvictionSubscription {
    fn drop(&mut self) {
        self.state.lock().remove(&self.asset_root, self.id);
    }
}
