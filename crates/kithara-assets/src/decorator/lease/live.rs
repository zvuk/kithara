use std::sync::Weak;

use dashmap::DashMap;
use kithara_platform::sync::{Arc, Mutex};

use crate::{layout::ResourceKey, resource::AssetResourceState};

/// Live lease resources keyed by their self-identifying resource key.
pub(super) type LiveRegistry = DashMap<ResourceKey, Weak<LiveResource>>;
pub(super) type RemoveFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

pub(super) struct LiveResource {
    state: Mutex<AssetResourceState>,
    key: ResourceKey,
    registry: Weak<LiveRegistry>,
}

impl LiveResource {
    pub(super) fn new(
        key: ResourceKey,
        registry: Weak<LiveRegistry>,
        state: AssetResourceState,
    ) -> Self {
        Self {
            key,
            registry,
            state: Mutex::new(state),
        }
    }

    pub(super) fn set(&self, state: AssetResourceState) {
        *self.state.lock() = state;
    }

    pub(super) fn snapshot(&self) -> AssetResourceState {
        self.state.lock().clone()
    }
}

impl Drop for LiveResource {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        registry.remove_if(&self.key, |_, weak| weak.upgrade().is_none());
    }
}
