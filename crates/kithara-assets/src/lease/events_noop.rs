use crate::key::ResourceKey;

#[derive(Clone, Default)]
pub(crate) struct EventsHandle;

impl EventsHandle {
    pub(crate) fn new(_bus: Option<()>) -> Self {
        Self
    }

    pub(super) fn publish_committed(&self, _key: Option<&ResourceKey>, _final_len: Option<u64>) {}

    pub(super) fn publish_failed(&self, _key: Option<&ResourceKey>, _reason: &str) {}
}
