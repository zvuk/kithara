#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::timeline::TimelineFlags;

/// Read-only observations of seek/flush coordination state.
pub(crate) trait SeekObserve: Send + Sync {
    fn epoch(&self) -> u64;
    fn is_flushing(&self) -> bool;
    fn is_pending(&self) -> bool;
    fn target(&self) -> Option<Duration>;
    fn take_preempt(&self) -> bool;
    fn take_decoder_seek(&self) -> bool;
    fn pending_epoch(&self) -> Option<u64>;
    fn clear_pending_epoch(&self, epoch: u64) -> bool;
}

/// Mutating seek coordination — `FLUSH_START` / `FLUSH_STOP` protocol.
pub(crate) trait SeekControl: Send + Sync {
    fn begin(&self, target: Duration) -> u64;
    fn complete(&self, epoch: u64);
    fn clear_pending(&self, epoch: u64);
    fn mark_pending(&self, epoch: u64);
}

/// Activity flag — whether the audio FSM has this timeline as active decode target.
pub(crate) trait Activity: Send + Sync {
    fn is_playing(&self) -> bool;
    fn set_playing(&self, playing: bool);
}

/// Concrete owner of the six seek/activity atomics.
///
/// Implements `SeekObserve`, `SeekControl`, and `Activity`.
/// `Timeline` holds an `Arc<SeekState>` and delegates all seek/flag/activity
/// methods to it.
#[derive(Debug)]
pub(crate) struct SeekState {
    /// Kept as `Arc<AtomicU64>` so `Timeline::seek_epoch_handle` can
    /// hand out a cheap `Arc` clone — same pattern as the original field.
    seek_epoch: Arc<AtomicU64>,
    seek_target_ns: AtomicU64,
    pending_seek_epoch: AtomicU64,
    /// Consolidated boolean state: `FLUSHING`, `SEEK_PENDING`, `PLAYING`.
    flags: AtomicU8,
    /// Hot-path latch: set by `begin`, consumed once by the audio worker's
    /// `swap(false, Acquire)`. The Release in `begin` (after epoch/target
    /// stores) synchronises with the Acquire in `take_preempt` — seeing
    /// `true` here guarantees the new epoch and target are visible.
    seek_preempt_latch: AtomicBool,
    /// Independent one-shot for `DecoderNode::sync_seek_epoch`. Armed by
    /// `begin` together with `seek_preempt_latch`.
    decoder_node_seek_latch: AtomicBool,
}

impl SeekState {
    const NO_PENDING_SEEK: u64 = u64::MAX;
    const NO_SEEK_TARGET: u64 = u64::MAX;

    // ast-grep-ignore: style.prefer-default-derive
    pub(crate) fn new() -> Self {
        Self {
            seek_epoch: Arc::new(AtomicU64::new(0)),
            seek_target_ns: AtomicU64::new(Self::NO_SEEK_TARGET),
            pending_seek_epoch: AtomicU64::new(Self::NO_PENDING_SEEK),
            flags: AtomicU8::new(TimelineFlags::empty().bits()),
            seek_preempt_latch: AtomicBool::new(false),
            decoder_node_seek_latch: AtomicBool::new(false),
        }
    }

    /// Expose the raw seek-epoch `Arc` for callers that need to share
    /// the atomic directly (e.g. `Timeline::seek_epoch_handle`).
    pub(crate) fn seek_epoch_arc(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.seek_epoch)
    }

    /// Raw flags load with caller-specified ordering — used by the
    /// timeline's `flags_snapshot_with` in tests.
    #[cfg(test)]
    pub(crate) fn flags_raw(&self, order: Ordering) -> u8 {
        self.flags.load(order)
    }

    /// Clear specific flags with the given ordering — test-only helper
    /// for simulating interleaved concurrent operations.
    #[cfg(test)]
    pub(crate) fn remove_flags_raw(&self, flags: TimelineFlags, order: Ordering) {
        self.flags.fetch_and(!flags.bits(), order);
    }
}

impl SeekObserve for SeekState {
    fn epoch(&self) -> u64 {
        self.seek_epoch.load(Ordering::Acquire)
    }

    fn is_flushing(&self) -> bool {
        TimelineFlags::from_bits_truncate(self.flags.load(Ordering::Acquire))
            .contains(TimelineFlags::FLUSHING)
    }

    fn is_pending(&self) -> bool {
        TimelineFlags::from_bits_truncate(self.flags.load(Ordering::Acquire))
            .contains(TimelineFlags::SEEK_PENDING)
    }

    fn target(&self) -> Option<Duration> {
        let ns = self.seek_target_ns.load(Ordering::Acquire);
        if ns == Self::NO_SEEK_TARGET {
            None
        } else {
            Some(Duration::from_nanos(ns))
        }
    }

    fn take_preempt(&self) -> bool {
        self.seek_preempt_latch.swap(false, Ordering::Acquire)
    }

    fn take_decoder_seek(&self) -> bool {
        self.decoder_node_seek_latch.swap(false, Ordering::Acquire)
    }

    fn pending_epoch(&self) -> Option<u64> {
        let epoch = self.pending_seek_epoch.load(Ordering::Acquire);
        if epoch == Self::NO_PENDING_SEEK {
            None
        } else {
            Some(epoch)
        }
    }

    fn clear_pending_epoch(&self, epoch: u64) -> bool {
        self.pending_seek_epoch
            .compare_exchange(
                epoch,
                Self::NO_PENDING_SEEK,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

impl SeekControl for SeekState {
    /// Initiate a seek (`FLUSH_START`).
    ///
    /// Sets flushing flag, records target position, increments epoch.
    /// All blocking reads (`wait_range`) will observe `is_flushing()` and abort.
    ///
    /// Returns the new seek epoch.
    ///
    /// # Panics
    /// Panics if `target` overflows `u64::MAX` nanoseconds (≈584 years —
    /// not reachable for any realistic seek target).
    fn begin(&self, target: Duration) -> u64 {
        let nanos = u64::try_from(target.as_nanos())
            .expect("BUG: initiate_seek target.as_nanos() fits in u64 for any realistic Duration");
        let epoch = self.seek_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        self.seek_target_ns.store(nanos, Ordering::Release);
        // NOTE: do NOT pre-set `committed_position` to `target` here.
        self.flags
            .fetch_or(TimelineFlags::SEEK_PENDING.bits(), Ordering::Release);
        self.flags
            .fetch_or(TimelineFlags::FLUSHING.bits(), Ordering::Release);
        self.seek_preempt_latch.store(true, Ordering::Release);
        self.decoder_node_seek_latch.store(true, Ordering::Release);
        epoch
    }

    /// Complete a seek (`FLUSH_STOP`).
    ///
    /// Clears flushing flag only if `epoch` is still current.
    /// A superseding `begin` will have incremented the epoch,
    /// preventing an older completion from clearing the new seek.
    ///
    /// Uses a double-check to guard against the race where a new
    /// `begin` fires between our epoch load and flushing store.
    fn complete(&self, epoch: u64) {
        if self.seek_epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        // NOTE: we do NOT clear seek_target_ns here.
        self.flags
            .fetch_and(!TimelineFlags::FLUSHING.bits(), Ordering::SeqCst);
        if self.seek_epoch.load(Ordering::SeqCst) != epoch {
            self.flags
                .fetch_or(TimelineFlags::FLUSHING.bits(), Ordering::SeqCst);
        }
    }

    /// Clear seek-pending flag after the decoder successfully applied the seek.
    ///
    /// Only clears if `epoch` matches the current seek epoch, preventing a
    /// stale completion from clearing a newer seek.
    fn clear_pending(&self, epoch: u64) {
        if self.seek_epoch.load(Ordering::Acquire) == epoch {
            self.flags
                .fetch_and(!TimelineFlags::SEEK_PENDING.bits(), Ordering::Release);
        }
    }

    fn mark_pending(&self, epoch: u64) {
        self.pending_seek_epoch.store(epoch, Ordering::Release);
    }
}

impl Activity for SeekState {
    fn is_playing(&self) -> bool {
        TimelineFlags::from_bits_truncate(self.flags.load(Ordering::Acquire))
            .contains(TimelineFlags::PLAYING)
    }

    /// Toggle the `PLAYING` flag.
    ///
    /// Orthogonal to `FLUSHING` / `SEEK_PENDING`: toggling `PLAYING`
    /// does not affect the seek state.
    fn set_playing(&self, playing: bool) {
        if playing {
            self.flags
                .fetch_or(TimelineFlags::PLAYING.bits(), Ordering::Release);
        } else {
            self.flags
                .fetch_and(!TimelineFlags::PLAYING.bits(), Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn state() -> SeekState {
        SeekState::new()
    }

    /// `begin` returns strictly increasing epochs.
    #[kithara::test]
    fn epoch_monotonicity() {
        let s = state();
        let e1 = s.begin(Duration::from_secs(1));
        let e2 = s.begin(Duration::from_secs(2));
        let e3 = s.begin(Duration::from_secs(3));
        assert_eq!(e1, 1);
        assert_eq!(e2, 2);
        assert_eq!(e3, 3);
        assert_eq!(s.epoch(), 3);
    }

    /// `complete(old_epoch)` must NOT clear a newer seek's flushing flag.
    #[kithara::test]
    fn stale_complete_leaves_newer_seek_intact() {
        let s = state();
        let e1 = s.begin(Duration::from_secs(5));
        let _e2 = s.begin(Duration::from_secs(10));
        s.complete(e1);
        assert!(
            s.is_flushing(),
            "newer seek's FLUSHING must survive a stale complete"
        );
        assert_eq!(s.target(), Some(Duration::from_secs(10)));
    }

    /// `take_preempt` returns `true` exactly once after `begin`, then `false`.
    #[kithara::test]
    fn latch_one_shot() {
        let s = state();
        s.begin(Duration::from_secs(3));
        assert!(s.take_preempt(), "first take must be true");
        assert!(!s.take_preempt(), "second take must be false");
        assert!(!s.take_preempt(), "subsequent takes must be false");
    }
}
