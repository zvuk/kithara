#[derive(Clone, Default)]
pub(crate) struct EventsHandle;

impl EventsHandle {
    pub(crate) fn new(_bus: Option<()>) -> Self {
        Self
    }

    pub(super) fn publish_evicted_bytes(&self, _published: bool, _asset_root: &str) {}

    pub(super) fn publish_evicted_assets(&self, _published: bool, _asset_root: &str) {}
}
