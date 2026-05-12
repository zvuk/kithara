#![forbid(unsafe_code)]

//! Crate-private one-slot replace-or-clear demand mailbox.
//!
//! Inlined from `kithara-stream`'s legacy `DemandSlot` so the upstream
//! crate stays free of HLS/file-specific helpers. Plan 07 (file Source
//! rewrite) revisits this when `FileSession` owns its own `position`
//! atomic.

use std::sync::Arc;

use kithara_platform::Mutex;

pub(crate) struct DemandSlot<D>
where
    D: Clone + Send + Sync + 'static,
{
    inner: Arc<Mutex<Option<D>>>,
}

impl<D> DemandSlot<D>
where
    D: Clone + Send + Sync + 'static,
{
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn did_replace(&self, demand: D) -> bool
    where
        D: PartialEq,
    {
        let mut slot = self.inner.lock_sync();
        if slot.as_ref() == Some(&demand) {
            return false;
        }
        *slot = Some(demand);
        true
    }

    #[must_use]
    pub(crate) fn take(&self) -> Option<D> {
        self.inner.lock_sync().take()
    }
}

impl<D> Clone for DemandSlot<D>
where
    D: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<D> Default for DemandSlot<D>
where
    D: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
