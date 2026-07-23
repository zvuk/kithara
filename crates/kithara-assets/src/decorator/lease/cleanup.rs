use kithara_platform::sync::Arc;

use super::{
    layer::LeaseEvents,
    live::{LiveResource, RemoveFn},
};
use crate::{layout::ResourceKey, resource::AssetResourceState};

/// Cleanup hook for an abandoned write handle. Lives as a field so the writer
/// has no `Drop` impl, allowing its terminal operations to move the inner
/// handle. A committed live resource is never removed by this hook.
pub(super) struct WriterCleanup {
    drop_token: Option<Arc<()>>,
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
            drop_token: Some(Arc::new(())),
            events,
            live,
            remove,
            resource_key,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WriterCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(live) = &self.live
            && matches!(live.snapshot(), AssetResourceState::Committed { .. })
        {
            return;
        }
        if let (Some(remove), Some(key)) = (&self.remove, &self.resource_key) {
            if self
                .drop_token
                .as_ref()
                .is_some_and(|token| Arc::strong_count(token) > 1)
            {
                return;
            }
            remove(key);
        }
    }
}
