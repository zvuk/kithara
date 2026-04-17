//! Types for the shared audio worker.

use std::sync::atomic::{AtomicU64, Ordering};

/// Unique identifier for a track registered with a shared worker.
pub(crate) type TrackId = u64;

/// Monotonic counter for generating unique [`TrackId`] values.
pub(crate) struct TrackIdGen(AtomicU64);

impl TrackIdGen {
    pub(crate) fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    pub(crate) fn next(&self) -> TrackId {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

pub use kithara_rt::ServiceClass;

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn track_id_gen_produces_unique_ids() {
        let id_gen = TrackIdGen::new();
        let a = id_gen.next();
        let b = id_gen.next();
        let c = id_gen.next();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
    }
}
