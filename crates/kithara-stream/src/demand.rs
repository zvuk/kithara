#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara_platform::Mutex;

pub struct DemandSlot<D>
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
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub fn submit(&self, demand: D) -> bool
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

    pub fn replace(&self, demand: D) {
        *self.inner.lock_sync() = Some(demand);
    }

    #[must_use]
    pub fn peek(&self) -> Option<D> {
        self.inner.lock_sync().clone()
    }

    #[must_use]
    pub fn take(&self) -> Option<D> {
        self.inner.lock_sync().take()
    }

    pub fn clear(&self) {
        *self.inner.lock_sync() = None;
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
