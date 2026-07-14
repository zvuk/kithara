use kithara_events::{AssetEvent, EventBus};

use crate::key::ResourceKey;

#[derive(Clone, Default)]
pub(crate) struct EventsHandle {
    bus: Option<EventBus>,
}

impl EventsHandle {
    pub(crate) fn new(bus: Option<EventBus>) -> Self {
        Self { bus }
    }

    pub(super) fn publish_committed(&self, key: Option<&ResourceKey>, final_len: Option<u64>) {
        let Some(bus) = &self.bus else {
            return;
        };
        let Some((asset_root, rel_path)) = key.and_then(|key| key.asset_root().zip(key.rel_path()))
        else {
            return;
        };
        bus.publish(AssetEvent::Committed {
            asset_root: asset_root.to_string(),
            rel_path: rel_path.to_string(),
            final_len,
        });
    }

    pub(super) fn publish_failed(&self, key: Option<&ResourceKey>, reason: &str) {
        let Some(bus) = &self.bus else {
            return;
        };
        let Some((asset_root, rel_path)) = key.and_then(|key| key.asset_root().zip(key.rel_path()))
        else {
            return;
        };
        bus.publish(AssetEvent::Failed {
            asset_root: asset_root.to_string(),
            rel_path: rel_path.to_string(),
            reason: reason.to_string(),
        });
    }
}
