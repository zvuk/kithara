use kithara_platform::sync::Arc;

use super::{
    layer::LeaseEvents,
    live::{LiveResource, RemoveFn},
};
use crate::{layout::ResourceKey, resource::AssetResourceState};

/// Cleanup hook for an abandoned write handle. Lives as a field so the writer
/// has no `Drop` impl, allowing its terminal operations to move the inner
/// handle. A committed resource, or one another handle still views, stays: the
/// next acquire resumes it through that shared resource and its tmp claim.
pub(super) struct WriterCleanup {
    pub(super) events: LeaseEvents,
    pub(super) live: Option<Arc<LiveResource>>,
    pub(super) remove: Option<RemoveFn>,
    pub(super) resource_key: Option<ResourceKey>,
    armed: bool,
}

impl WriterCleanup {
    pub(super) fn new(
        events: LeaseEvents,
        live: Option<Arc<LiveResource>>,
        remove: Option<RemoveFn>,
        resource_key: Option<ResourceKey>,
    ) -> Self {
        Self {
            events,
            live,
            remove,
            resource_key,
            armed: true,
        }
    }

    pub(super) const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WriterCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.live.as_ref().is_some_and(|live| {
            Arc::strong_count(live) > 1
                || matches!(live.snapshot(), AssetResourceState::Committed { .. })
        }) {
            return;
        }
        if let (Some(remove), Some(key)) = (&self.remove, &self.resource_key) {
            remove(key);
        }
    }
}
