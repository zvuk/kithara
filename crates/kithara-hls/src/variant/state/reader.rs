use std::sync::atomic::{AtomicU64, Ordering};

use kithara_platform::sync::Arc;
use kithara_stream::SeekObserve;

/// Reader-side runtime state for one variant: the shared byte cursor and the
/// seek-observe handle consulted for `is_flushing` gating.
///
/// Grouped out of `HlsVariant` so the cursor and flush-gate live in one domain.
/// The variant facade keeps the probe-tagged public accessors (`get_position` /
/// `set_position` carry the `variant`-tagged USDT probe) and delegates the raw
/// ops here.
pub(super) struct ReaderRuntime {
    position: Arc<AtomicU64>,
    seek_obs: Arc<dyn SeekObserve>,
}

impl ReaderRuntime {
    pub(super) fn new(seek_obs: Arc<dyn SeekObserve>) -> Self {
        Self {
            seek_obs,
            position: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(super) fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    pub(super) fn is_flushing(&self) -> bool {
        self.seek_obs.is_flushing()
    }

    pub(super) fn is_seek_active(&self) -> bool {
        self.seek_obs.is_flushing() || self.seek_obs.is_pending()
    }

    pub(super) fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    pub(super) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }
}
