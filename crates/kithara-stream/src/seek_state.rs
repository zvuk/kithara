#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use bitflags::bitflags;
use kithara_platform::time::Duration;

bitflags! {
    /// Boolean playback-state flags stored in a single `AtomicU8` on [`SeekState`].
    ///
    /// Consolidated into one atomic so readers (HLS peer priority, reader
    /// wait loops, audio FSM) observe a coherent snapshot with a single
    /// load and writers compose flag updates with `fetch_or` / `fetch_and`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct TimelineFlags: u8 {
        /// Audio FSM playback-activity writer.
        const PLAYING      = 1 << 0;
        /// Pipeline is being flushed (seek in progress); gates `wait_range` I/O.
        const FLUSHING     = 1 << 1;
        /// Seek initiated but the decoder has not yet repositioned.
        const SEEK_PENDING = 1 << 2;
    }
}

/// Read-only observations of seek/flush coordination state.
pub trait SeekObserve: Send + Sync {
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
pub trait SeekControl: Send + Sync {
    fn begin(&self, target: Duration) -> u64;
    fn complete(&self, epoch: u64);
    fn clear_pending(&self, epoch: u64);
    fn mark_pending(&self, epoch: u64);
}

/// Activity flag — whether the audio FSM has this timeline as active decode target.
pub trait Activity: Send + Sync {
    fn is_playing(&self) -> bool;
    fn set_playing(&self, playing: bool);
}

/// Concrete owner of the six seek/activity atomics.
///
/// Implements `SeekObserve`, `SeekControl`, and `Activity`. Coords hold an
/// `Arc<SeekState>` and vend the narrow trait handles to readers/writers.
#[derive(Debug)]
pub struct SeekState {
    /// Kept as `Arc<AtomicU64>` so callers can hand out a cheap `Arc`
    /// clone of the shared seek epoch atomic.
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

    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
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
    /// the atomic directly (e.g. a shared seek-epoch handle).
    pub fn seek_epoch_arc(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.seek_epoch)
    }

    /// Raw flags load with caller-specified ordering — used by the
    /// flag-snapshot tests below.
    #[cfg(test)]
    fn flags_raw(&self, order: Ordering) -> u8 {
        self.flags.load(order)
    }

    /// Clear specific flags with the given ordering — test-only helper
    /// for simulating interleaved concurrent operations.
    #[cfg(test)]
    fn remove_flags_raw(&self, flags: TimelineFlags, order: Ordering) {
        self.flags.fetch_and(!flags.bits(), order);
    }
}

impl Default for SeekState {
    fn default() -> Self {
        Self::new()
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
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use kithara_test_utils::kithara;

    use super::*;

    fn state() -> SeekState {
        SeekState::new()
    }

    fn flags_snapshot(s: &SeekState, order: Ordering) -> TimelineFlags {
        TimelineFlags::from_bits_truncate(s.flags_raw(order))
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

    #[kithara::test]
    fn initiate_seek_sets_flushing_and_target() {
        let s = state();
        assert!(!s.is_flushing());
        assert!(s.target().is_none());

        let epoch = s.begin(Duration::from_secs(10));
        assert_eq!(epoch, 1);
        assert!(s.is_flushing());
        assert_eq!(s.target(), Some(Duration::from_secs(10)));
        assert_eq!(s.epoch(), 1);
    }

    #[kithara::test]
    fn complete_seek_clears_flushing() {
        let s = state();
        let epoch = s.begin(Duration::from_secs(5));
        s.complete(epoch);
        assert!(!s.is_flushing());
        assert_eq!(s.target(), Some(Duration::from_secs(5)));
    }

    #[kithara::test]
    fn complete_seek_ignores_stale_epoch() {
        let s = state();
        let epoch1 = s.begin(Duration::from_secs(5));
        let epoch2 = s.begin(Duration::from_secs(10));
        s.complete(epoch1);
        assert!(s.is_flushing());
        assert_eq!(s.target(), Some(Duration::from_secs(10)));
        s.complete(epoch2);
        assert!(!s.is_flushing());
    }

    #[kithara::test]
    fn seek_epoch_monotonically_increases() {
        let s = state();
        let e1 = s.begin(Duration::from_secs(1));
        let e2 = s.begin(Duration::from_secs(2));
        let e3 = s.begin(Duration::from_secs(3));
        assert_eq!(e1, 1);
        assert_eq!(e2, 2);
        assert_eq!(e3, 3);
        assert_eq!(s.target(), Some(Duration::from_secs(3)));
    }

    #[kithara::test]
    fn complete_seek_does_not_clobber_concurrent_target() {
        let s = state();
        let epoch1 = s.begin(Duration::from_secs(5));
        let _epoch2 = s.begin(Duration::from_secs(15));
        s.complete(epoch1);
        assert!(s.is_flushing());
        assert_eq!(s.target(), Some(Duration::from_secs(15)));
    }

    #[kithara::test]
    fn initiate_seek_is_visible_across_arc_clones() {
        let s = Arc::new(state());
        let clone = Arc::clone(&s);
        let _ = s.begin(Duration::from_secs(7));
        assert!(clone.is_flushing());
        assert_eq!(clone.target(), Some(Duration::from_secs(7)));
    }

    #[kithara::test]
    fn initiate_seek_sets_seek_pending() {
        let s = state();
        assert!(!s.is_pending());
        let _epoch = s.begin(Duration::from_secs(5));
        assert!(s.is_pending());
    }

    #[kithara::test]
    fn clear_seek_pending_only_clears_matching_epoch() {
        let s = state();
        let epoch1 = s.begin(Duration::from_secs(5));
        let epoch2 = s.begin(Duration::from_secs(10));
        s.clear_pending(epoch1);
        assert!(s.is_pending());
        s.clear_pending(epoch2);
        assert!(!s.is_pending());
    }

    #[kithara::test]
    fn new_initiate_seek_resets_seek_pending() {
        let s = state();
        let epoch = s.begin(Duration::from_secs(5));
        s.clear_pending(epoch);
        assert!(!s.is_pending());
        let _epoch2 = s.begin(Duration::from_secs(10));
        assert!(s.is_pending());
    }

    #[kithara::test]
    fn complete_seek_does_not_clear_seek_pending() {
        let s = state();
        let epoch = s.begin(Duration::from_secs(5));
        s.complete(epoch);
        assert!(!s.is_flushing());
        assert!(s.is_pending());
    }

    #[kithara::test]
    fn is_seek_pending_visible_across_arc_clones() {
        let s = Arc::new(state());
        let clone = Arc::clone(&s);
        let _epoch = s.begin(Duration::from_secs(5));
        assert!(clone.is_pending());
    }

    #[kithara::test]
    fn flag_pair_matrix_matches_bitflags_snapshot() {
        for mask in 0u8..4 {
            let s = state();
            let want_flushing = mask & 1 != 0;
            let want_seek_pending = mask & 2 != 0;

            if want_flushing || want_seek_pending {
                let _ = s.begin(Duration::from_secs(1));
                if !want_flushing {
                    s.complete(s.epoch());
                }
                if !want_seek_pending {
                    s.clear_pending(s.epoch());
                }
            }

            assert_eq!(s.is_flushing(), want_flushing, "mask {mask:#04b} flushing");
            assert_eq!(
                s.is_pending(),
                want_seek_pending,
                "mask {mask:#04b} seek_pending"
            );

            let snapshot = flags_snapshot(&s, Ordering::Acquire);
            assert_eq!(
                snapshot.contains(TimelineFlags::FLUSHING),
                want_flushing,
                "mask {mask:#04b} snapshot flushing"
            );
            assert_eq!(
                snapshot.contains(TimelineFlags::SEEK_PENDING),
                want_seek_pending,
                "mask {mask:#04b} snapshot seek_pending"
            );
        }
    }

    #[kithara::test]
    fn complete_seek_double_check_re_raises_flushing_when_newer_seek_interleaves() {
        let s = state();
        let epoch1 = s.begin(Duration::from_secs(1));

        s.remove_flags_raw(TimelineFlags::FLUSHING, Ordering::SeqCst);
        let _epoch2 = s.begin(Duration::from_secs(2));
        s.complete(epoch1);

        assert!(
            s.is_flushing(),
            "FLUSHING must be re-raised when a newer seek interleaves mid-complete"
        );
    }

    #[kithara::test]
    fn concurrent_flag_toggles_preserve_independent_semantics() {
        const ITER: usize = 50_000;

        let s = Arc::new(state());
        let barrier = Arc::new(Barrier::new(3));

        let s_a = Arc::clone(&s);
        let barrier_a = Arc::clone(&barrier);
        let a = thread::spawn(move || {
            barrier_a.wait();
            for i in 0..ITER {
                s_a.set_playing(i % 2 == 0);
            }
        });

        let s_b = Arc::clone(&s);
        let barrier_b = Arc::clone(&barrier);
        let b = thread::spawn(move || {
            barrier_b.wait();
            for _ in 0..ITER {
                let epoch = s_b.begin(Duration::from_millis(1));
                s_b.clear_pending(epoch);
                s_b.complete(epoch);
            }
        });

        let s_c = Arc::clone(&s);
        let barrier_c = Arc::clone(&barrier);
        let c = thread::spawn(move || {
            barrier_c.wait();
            let mut observed = 0u64;
            for _ in 0..ITER {
                let snap = flags_snapshot(&s_c, Ordering::Acquire);
                observed ^= u64::from(snap.bits());
            }
            observed
        });

        a.join()
            .expect("BUG: spawned thread A must not panic in this test");
        b.join()
            .expect("BUG: spawned thread B must not panic in this test");
        let _ = c
            .join()
            .expect("BUG: spawned thread C must not panic in this test");

        assert!(
            !s.is_playing(),
            "PLAYING must match the last deterministic write"
        );
        assert!(!s.is_flushing(), "FLUSHING must be fully cleared");
        assert!(
            !s.is_pending(),
            "SEEK_PENDING must be fully cleared after last clear"
        );
    }

    #[kithara::test]
    fn playing_defaults_to_false() {
        let s = state();
        assert!(!s.is_playing());
    }

    #[kithara::test]
    fn set_playing_true_is_visible_across_arc_clones() {
        let s = Arc::new(state());
        let clone = Arc::clone(&s);
        s.set_playing(true);
        assert!(clone.is_playing());
        clone.set_playing(false);
        assert!(!s.is_playing());
    }

    #[kithara::test]
    fn set_playing_idempotent() {
        let s = state();
        s.set_playing(true);
        s.set_playing(true);
        assert!(s.is_playing());
        s.set_playing(false);
        s.set_playing(false);
        assert!(!s.is_playing());
    }

    #[kithara::test]
    fn playing_is_orthogonal_to_other_flags() {
        for mask in 0u8..4 {
            for &initial_playing in &[false, true] {
                let s = state();
                let want_flushing = mask & 1 != 0;
                let want_seek_pending = mask & 2 != 0;

                if want_flushing || want_seek_pending {
                    let _ = s.begin(Duration::from_secs(1));
                    if !want_flushing {
                        s.complete(s.epoch());
                    }
                    if !want_seek_pending {
                        s.clear_pending(s.epoch());
                    }
                }
                s.set_playing(initial_playing);

                assert_eq!(s.is_playing(), initial_playing);
                assert_eq!(
                    s.is_flushing(),
                    want_flushing,
                    "mask {mask:#04b} play={initial_playing} flushing"
                );
                assert_eq!(
                    s.is_pending(),
                    want_seek_pending,
                    "mask {mask:#04b} play={initial_playing} seek_pending"
                );

                s.set_playing(!initial_playing);
                assert_eq!(s.is_playing(), !initial_playing);
                assert_eq!(s.is_flushing(), want_flushing);
                assert_eq!(s.is_pending(), want_seek_pending);
            }
        }
    }

    #[kithara::test]
    fn initiate_seek_does_not_touch_playing() {
        let s = state();
        s.set_playing(true);
        let epoch = s.begin(Duration::from_secs(5));
        assert!(s.is_playing(), "PLAYING must not be affected by seek");
        s.complete(epoch);
        assert!(s.is_playing(), "PLAYING must survive complete_seek");
        s.clear_pending(epoch);
        assert!(s.is_playing(), "PLAYING must survive clear_seek_pending");
    }
}
