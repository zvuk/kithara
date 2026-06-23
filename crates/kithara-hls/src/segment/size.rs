use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// `bytes > 0` is known/complete: the EXACT flag is set iff the segment's
/// byte length has been established (HEAD-seeded with a positive estimate, or
/// shrunk to a committed `final_len`). Mirrors the old `size.load() > 0`
/// completeness convention.
const EXACT: u8 = 0b01;

/// A per-segment byte length paired with a validity flag, replacing the bare
/// `AtomicU64` + the `> 0`-means-known convention. `get()` returns the raw
/// byte value (seed or committed); `is_exact()` answers the completeness gate.
///
/// **R1 ordering:** every mutator stores `bytes` (Release) **strictly before**
/// touching `flags`, and every reader loads with Acquire — so a thread that
/// observes the EXACT flag also observes the byte store that precedes it.
#[derive(Debug)]
pub(crate) struct SegmentSize {
    bytes: AtomicU64,
    flags: AtomicU8,
}

impl Default for SegmentSize {
    /// Empty/unknown: `bytes = 0`, no flags (mirrors `AtomicU64::new(0)`).
    fn default() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            flags: AtomicU8::new(0),
        }
    }
}

impl SegmentSize {
    /// Seed the size from a HEAD estimate (or the cumulative-offset table).
    /// Stores `n` then sets EXACT iff `n > 0` — preserving the parity that a
    /// non-zero seed counts as "known" exactly as the old `size > 0` did.
    pub(crate) fn seed(n: u64) -> Self {
        let size = Self::default();
        size.bytes.store(n, Ordering::Release);
        if n > 0 {
            size.flags.fetch_or(EXACT, Ordering::Release);
        }
        size
    }

    /// Store the committed/loaded byte length and mark it EXACT. Always called
    /// with a real `final_len`. Byte store (Release) strictly before the flag
    /// store (Release) — R1.
    pub(crate) fn set_exact(&self, n: u64) {
        self.bytes.store(n, Ordering::Release);
        self.flags.store(EXACT, Ordering::Release);
    }

    /// Raw byte value (seed or committed). Used for offset / total math.
    pub(crate) fn get(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }

    /// Whether the byte length is known. Identical to the old `size > 0`
    /// completeness predicate at this stage.
    pub(crate) fn is_exact(&self) -> bool {
        self.flags.load(Ordering::Acquire) & EXACT != 0
    }
}
