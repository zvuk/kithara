#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(u64);

impl SlotId {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

/// Monotonic identifier for a track across the entire process.
///
/// Allocated from a single global counter so [`Queue`](crate::queue) and
/// FFI items share one address space — the value `audioId` reports
/// over the FFI boundary is exactly the value the queue uses
/// internally. Stable across removals: removing a track and adding a
/// new one yields a fresh id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

impl core::fmt::Display for TrackId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<u64> for TrackId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<TrackId> for u64 {
    fn from(id: TrackId) -> Self {
        id.0
    }
}

impl TrackId {
    /// Allocate the next monotonic id from the process-wide counter.
    ///
    /// This is the single allocation site: the FFI item layer reserves
    /// an id at construction so caller-visible `audioId` is stable from
    /// day one, and `Queue::insert` consumes that same id without
    /// re-allocating. The counter starts at `0` and is never reset.
    #[must_use]
    pub fn allocate() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Raw id value.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}
