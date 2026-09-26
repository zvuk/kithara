#![forbid(unsafe_code)]

use std::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use crossbeam_queue::ArrayQueue;
use kithara_platform::sync::Arc;
use rangemap::RangeSet;
use tracing::warn;

/// Availability snapshots parked by produce-core reads for off-RT reclaim.
///
/// `write_at` publishes a fresh generation on every write, so a
/// `contains_range` read racing a writer can end up the last owner of the
/// generation it loaded — and dropping it there frees the range tree on the
/// audio thread (`RTSan`: unsafe-library-call in `free`, reached through
/// `ReadinessGate::source_ready_for_range`). Readers therefore never drop a
/// snapshot they own: they park it here, and the write side — `write_at` and
/// the commit/seal path, which publish the generations in the first place —
/// drains the bin and pays the frees.
///
/// Overflow leaks the reference instead of freeing it on the reader, mirroring
/// the availability-index bin - and, like that bin, it leaks under ordinary
/// playback rather than only at the margins: parks follow *reads* at
/// produce-tick cadence, drains follow *writes*, and nothing bounds that
/// ratio. `mem::forget` makes each overflow permanent.
pub(super) struct Retired {
    snapshots: ArrayQueue<Arc<RangeSet<u64>>>,
    overflowed: AtomicBool,
}

impl Retired {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            snapshots: ArrayQueue::new(capacity),
            overflowed: AtomicBool::new(false),
        }
    }

    pub(super) fn drain(&self) {
        while self.snapshots.pop().is_some() {}
        if self.overflowed.swap(false, Ordering::AcqRel) {
            warn!("availability retire bin overflowed; leaked snapshots to keep the reader free");
        }
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    pub(super) fn retire(&self, snapshot: Arc<RangeSet<u64>>) {
        if let Err(snapshot) = self.snapshots.push(snapshot) {
            self.overflowed.store(true, Ordering::Release);
            mem::forget(snapshot);
        }
    }
}
