#![forbid(unsafe_code)]

use std::{
    mem,
    sync::atomic::{AtomicBool, Ordering},
};

use crossbeam_queue::ArrayQueue;
use kithara_platform::sync::Arc;
use tracing::warn;

use super::core::{AssetTree, Availability};

/// Snapshot references parked by produce-core reads for off-RT reclaim.
///
/// A snapshot guard dropped on the audio thread can be the last owner of a
/// generation a writer has already replaced, and freeing the tree there is a
/// real-time violation (`RTSan`: unsafe-library-call in `AvailabilityIndex`
/// reads). Readers therefore never drop a snapshot they own: they park it
/// here, and the write side — the index's download and deletion paths, which
/// already publish the generations — drains the bin and pays the frees.
/// Overflow leaks the reference instead of freeing on the reader, mirroring
/// the decode retire queue. That leak is unrecoverable - `mem::forget` inflates
/// the strong count for good - and it is not rare: the park rate follows
/// *reads* (two per read, at audio-tick cadence) while the drain rate follows
/// *writes*, and nothing bounds that ratio. Playback served from cache issues
/// thousands of reads between two downloads and overflows on every one of them
/// past the first 128 (measured 2026-08-20: 844 overflow warnings in two
/// minutes of HLS playback). Bounding it needs the reader to stop taking
/// ownership per read - see `a_read_burst_never_leaks_a_generation`.
pub(super) struct Retired {
    availabilities: ArrayQueue<Arc<Availability>>,
    trees: ArrayQueue<Arc<AssetTree>>,
    overflowed: AtomicBool,
}

impl Retired {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            trees: ArrayQueue::new(capacity),
            availabilities: ArrayQueue::new(capacity),
            overflowed: AtomicBool::new(false),
        }
    }

    pub(super) fn drain(&self) {
        while self.trees.pop().is_some() {}
        while self.availabilities.pop().is_some() {}
        if self.overflowed.swap(false, Ordering::AcqRel) {
            warn!("availability retire bin overflowed; leaked snapshots to keep the reader free");
        }
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.trees.is_empty() && self.availabilities.is_empty()
    }

    /// Whether a park has leaked a generation since the last drain.
    #[cfg(test)]
    pub(super) fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::Acquire)
    }

    pub(super) fn retire_availability(&self, availability: Arc<Availability>) {
        if let Err(availability) = self.availabilities.push(availability) {
            self.overflowed.store(true, Ordering::Release);
            mem::forget(availability);
        }
    }

    pub(super) fn retire_tree(&self, tree: Arc<AssetTree>) {
        if let Err(tree) = self.trees.push(tree) {
            self.overflowed.store(true, Ordering::Release);
            mem::forget(tree);
        }
    }
}
