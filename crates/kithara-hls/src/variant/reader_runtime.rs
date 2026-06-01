use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};

use kithara_platform::tokio::sync::Notify;
use kithara_stream::Timeline;

/// Reader-side runtime state for one variant: the shared byte cursor, the
/// peer wake handle, and the timeline consulted for `is_flushing` gating.
///
/// Grouped out of `HlsVariant` so the cursor, wake, and flush-gate live in one
/// domain. The variant facade keeps the probe-tagged public accessors
/// (`get_position` / `set_position` carry the `variant`-tagged USDT probe) and
/// delegates the raw ops here.
pub(super) struct ReaderRuntime {
    position: Arc<AtomicU64>,
    /// Reader→peer wake handle, installed once when the owning `HlsPeer`
    /// binds. Empty until [`set_wake`](Self::set_wake) is called by `HlsCoord`.
    peer_wake: OnceLock<Arc<Notify>>,
    timeline: Timeline,
}

impl ReaderRuntime {
    pub(super) fn new(timeline: Timeline) -> Self {
        Self {
            position: Arc::new(AtomicU64::new(0)),
            peer_wake: OnceLock::new(),
            timeline,
        }
    }

    pub(super) fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    pub(super) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    pub(super) fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    pub(super) fn set_wake(&self, notify: Arc<Notify>) {
        let _ = self.peer_wake.set(notify);
    }

    pub(super) fn wake(&self) {
        if let Some(notify) = self.peer_wake.get() {
            notify.notify_one();
        }
    }

    pub(super) fn is_flushing(&self) -> bool {
        self.timeline.is_flushing()
    }
}
