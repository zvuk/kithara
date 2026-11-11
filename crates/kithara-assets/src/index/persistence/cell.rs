#![forbid(unsafe_code)]

use std::sync::{
    OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};

use kithara_platform::sync::Arc;

use super::{FlushHub, Flushable, IndexTarget};
use crate::error::AssetsResult;

/// What an index keeps in memory.
pub(crate) trait IndexState: Send + Sync + 'static {
    /// Name the flush hub reports for this index.
    const NAME: &'static str;
}

/// An index's state together with what every persisted index shares: the
/// dirty flag the [`FlushHub`] drains, the hub binding, and the file target.
/// Without a target the index is ephemeral and a flush writes nothing.
pub(crate) struct IndexCell<S> {
    pub(crate) state: S,
    pub(crate) dirty: AtomicBool,
    hub: OnceLock<Arc<FlushHub>>,
    pub(super) target: OnceLock<IndexTarget<S>>,
}

impl<S> IndexCell<S> {
    pub(crate) fn new(state: S) -> Self {
        Self {
            state,
            dirty: AtomicBool::new(false),
            hub: OnceLock::new(),
            target: OnceLock::new(),
        }
    }

    pub(crate) fn is_persisted(&self) -> bool {
        self.target.get().is_some()
    }

    pub(crate) fn hub(&self) -> Option<&Arc<FlushHub>> {
        self.hub.get()
    }

    /// Records a change for the next flush and wakes the hub, if bound.
    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
        if let Some(hub) = self.hub.get() {
            hub.signal();
        }
    }
}

impl<S: IndexState> IndexCell<S> {
    /// Binds the index to `hub` once; later calls keep the first binding.
    pub(crate) fn attach_to(self: &Arc<Self>, hub: &Arc<FlushHub>) {
        if self.hub.set(Arc::clone(hub)).is_ok() {
            hub.register(Arc::downgrade(self) as Weak<dyn Flushable>);
        }
    }

    fn write(&self, durable: bool) -> AssetsResult<()> {
        self.target
            .get()
            .map_or(Ok(()), |target| target.publish(&self.state, durable))
    }
}

/// The flush itself never clears `dirty`: the hub swaps it before flushing,
/// and a change that lands during the write must keep its signal.
impl<S: IndexState> Flushable for IndexCell<S> {
    fn dirty(&self) -> &AtomicBool {
        &self.dirty
    }

    fn flush(&self) -> AssetsResult<()> {
        self.write(false)
    }

    fn flush_durable(&self) -> AssetsResult<()> {
        self.write(true)
    }

    fn name(&self) -> &'static str {
        S::NAME
    }
}
