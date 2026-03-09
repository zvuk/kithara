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

/// Priority class for worker scheduling.
///
/// Tracks with higher service class are served first when the scheduler
/// selects which track to decode next.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ServiceClass {
    /// Not playing, not needed soon. Lowest priority.
    #[default]
    Idle,
    /// Preloading or about to play. Medium priority.
    Warm,
    /// Currently audible. Highest priority.
    Audible,
}

/// Per-track phase in the shared worker state machine.
///
/// ```text
///          ┌──────────┐
///     ┌───►│ Decoding  │◄──── is_range_ready + ringbuf has space
///     │    └────┬──────┘
///     │         │ seek / format change
///     │    ┌────▼──────────────┐
///     │    │ PendingReset      │ ← waiting for data to recreate decoder
///     │    └────┬──────────────┘
///     │         │ data ready → apply seek / recreate
///     └─────────┘
///
///     AtEof ── seek ──► PendingReset
///     Failed ── terminal, track will be removed
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TrackPhase {
    /// Normal decoding — `fetch_next()` when ready.
    #[default]
    Decoding,
    /// Waiting for data to apply seek or decoder recreation (ABR switch).
    PendingReset,
    /// End of stream reached.
    AtEof,
    /// Unrecoverable error (panic or fatal decode error).
    Failed,
}

/// Result of a single worker step for one track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepResult {
    /// Track made progress (decoded chunk, applied seek, pushed pending).
    Progress,
    /// Track could not progress (no data, ringbuf full, EOF, failed).
    NoProgress,
}

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

    #[kithara::test]
    fn service_class_ordering() {
        assert!(ServiceClass::Idle < ServiceClass::Warm);
        assert!(ServiceClass::Warm < ServiceClass::Audible);
    }

    #[kithara::test]
    fn track_phase_default_is_decoding() {
        assert_eq!(TrackPhase::default(), TrackPhase::Decoding);
    }

    #[kithara::test]
    fn service_class_default_is_idle() {
        assert_eq!(ServiceClass::default(), ServiceClass::Idle);
    }

    #[kithara::test]
    fn step_result_equality() {
        assert_eq!(StepResult::Progress, StepResult::Progress);
        assert_ne!(StepResult::Progress, StepResult::NoProgress);
    }
}
