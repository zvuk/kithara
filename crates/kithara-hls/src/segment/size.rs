use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The EXACT flag is set iff the segment's byte length has been established
/// (HEAD-seeded with a positive estimate, a byterange seed, or a committed
/// `final_len`). Non-exact placeholders may still carry a non-zero byte count
/// for routing, so completeness is the flag, not `bytes > 0`.
const EXACT: u8 = 0b01;

/// Per-segment byte lengths paired with a validity flag, replacing the bare
/// `AtomicU64` + the `> 0`-means-known convention. Route and read lengths are
/// stored separately so callers name the ownership boundary explicitly; commit
/// keeps them equal for the contiguous HLS byte map.
///
/// **R1 ordering:** every mutator stores `bytes` (Release) **strictly before**
/// touching `flags`, and every reader loads with Acquire — so a thread that
/// observes the EXACT flag also observes the byte store that precedes it.
#[derive(Debug)]
pub(crate) struct SegmentSize {
    route_bytes: AtomicU64,
    read_bytes: AtomicU64,
    flags: AtomicU8,
}

impl Default for SegmentSize {
    /// Empty/unknown: no route or read length, no flags.
    fn default() -> Self {
        Self {
            route_bytes: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            flags: AtomicU8::new(0),
        }
    }
}

impl SegmentSize {
    /// Route byte value (seed or placeholder). Used for offset / total math.
    pub(crate) fn get(&self) -> u64 {
        self.route_bytes.load(Ordering::Acquire)
    }

    /// Read byte value. Before exact resolution this mirrors the route value
    /// so descriptors are still routeable; after resolution it reports the
    /// committed/probed byte length.
    pub(crate) fn read_len(&self) -> u64 {
        if self.is_exact() {
            self.read_bytes.load(Ordering::Acquire)
        } else {
            self.get()
        }
    }

    /// Whether the byte length is known and can be used for readiness/EOF.
    pub(crate) fn is_exact(&self) -> bool {
        self.flags.load(Ordering::Acquire) & EXACT != 0
    }

    /// Seed a routeable size that is not yet exact. Used by segment-aware
    /// containers whose final size will be learned from the body commit.
    pub(crate) fn placeholder(n: u64) -> Self {
        let size = Self::default();
        size.route_bytes.store(n, Ordering::Release);
        size
    }

    /// Seed the size from an exact playlist/probe value.
    /// Stores `n` then sets EXACT iff `n > 0` — preserving the parity that a
    /// non-zero seed counts as "known" exactly as the old `size > 0` did.
    pub(crate) fn seed(n: u64) -> Self {
        let size = Self::default();
        size.route_bytes.store(n, Ordering::Release);
        size.read_bytes.store(n, Ordering::Release);
        if n > 0 {
            size.flags.fetch_or(EXACT, Ordering::Release);
        }
        size
    }

    /// Store the committed/loaded byte length and mark it EXACT. Always called
    /// with a real `final_len`. Byte store (Release) strictly before the flag
    /// store (Release) — R1.
    pub(crate) fn set_exact(&self, n: u64) {
        self.route_bytes.store(n, Ordering::Release);
        self.read_bytes.store(n, Ordering::Release);
        self.flags.store(EXACT, Ordering::Release);
    }

    /// Store a probe-resolved exact size only while no exact value exists.
    pub(crate) fn set_exact_if_unknown(&self, n: u64) -> bool {
        if n == 0 || self.is_exact() {
            return false;
        }
        self.set_exact(n);
        true
    }
}
