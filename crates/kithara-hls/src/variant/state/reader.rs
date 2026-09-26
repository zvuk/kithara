use std::sync::atomic::{AtomicU64, Ordering};

use kithara_platform::sync::Arc;
use kithara_stream::SeekObserve;

use crate::consts;

/// Seek-observe state consulted by one variant for flushing gates, plus the
/// byte end of the range the reader is currently parked on.
pub(super) struct ReaderRuntime {
    seek_obs: Arc<dyn SeekObserve>,
    /// End of the unready range the last `wait_range` parked on; `NO_WAIT`
    /// when the reader is not waiting. A parked reader names bytes the splice
    /// still consumes, so the owed dispatch reads this to size its window.
    wait_end: AtomicU64,
}

impl ReaderRuntime {
    pub(super) fn new(seek_obs: Arc<dyn SeekObserve>) -> Self {
        Self {
            seek_obs,
            wait_end: AtomicU64::new(consts::NO_WAIT),
        }
    }

    pub(super) fn clear_wait(&self) {
        self.wait_end.store(consts::NO_WAIT, Ordering::Release);
    }

    pub(super) fn is_flushing(&self) -> bool {
        self.seek_obs.is_flushing()
    }

    pub(super) fn is_seek_active(&self) -> bool {
        self.seek_obs.is_flushing() || self.seek_obs.is_pending()
    }

    pub(super) fn note_wait(&self, end: u64) {
        self.wait_end.store(end.max(1), Ordering::Release);
    }

    pub(super) fn wait_end(&self) -> Option<u64> {
        match self.wait_end.load(Ordering::Acquire) {
            consts::NO_WAIT => None,
            end => Some(end),
        }
    }
}
