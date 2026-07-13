use kithara_events::{AssetEvent, EventBus, EvictReason};

#[derive(Clone, Default)]
pub(crate) struct EventsHandle {
    bus: Option<EventBus>,
}

impl EventsHandle {
    pub(crate) fn new(bus: Option<EventBus>) -> Self {
        Self { bus }
    }

    pub(super) fn publish_evicted_bytes(&self, published: bool, asset_root: &str) {
        self.publish_evicted(published, asset_root, EvictReason::QuotaBytes);
    }

    pub(super) fn publish_evicted_assets(&self, published: bool, asset_root: &str) {
        self.publish_evicted(published, asset_root, EvictReason::QuotaAssets);
    }

    fn publish_evicted(&self, published: bool, asset_root: &str, reason: EvictReason) {
        if !published {
            return;
        }
        let Some(bus) = &self.bus else {
            return;
        };
        bus.publish(AssetEvent::Evicted {
            asset_root: asset_root.to_string(),
            reason,
        });
    }
}
