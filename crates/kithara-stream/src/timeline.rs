#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use bitflags::bitflags;

bitflags! {
    /// Boolean playback-state flags stored in a single `AtomicU8` on [`Timeline`].
    ///
    /// Consolidated into one atomic so readers (HLS peer priority, reader
    /// wait loops, audio FSM) observe a coherent snapshot with a single
    /// load and writers compose flag updates with `fetch_or` / `fetch_and`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TimelineFlags: u8 {
        /// Reserved for the audio FSM playback-activity writer (Task 4+).
        const PLAYING      = 1 << 0;
        /// Pipeline is being flushed (seek in progress); gates `wait_range` I/O.
        const FLUSHING     = 1 << 1;
        /// Seek initiated but the decoder has not yet repositioned.
        const SEEK_PENDING = 1 << 2;
        /// End of stream reached.
        const EOF          = 1 << 3;
    }
}

/// Shared playback timeline used across stream layers.
///
/// Stores canonical byte position and committed playback position.
#[derive(Clone, Debug)]
pub struct Timeline {
    byte_position: Arc<AtomicU64>,
    committed_position_ns: Arc<AtomicU64>,
    download_position: Arc<AtomicU64>,
    pending_seek_epoch: Arc<AtomicU64>,
    total_duration_ns: Arc<AtomicU64>,

    /// Byte offset at the start of the most recent `Stream::read()` call.
    /// Used by `StreamContext::segment_index()` to resolve which segment
    /// the last-read data belongs to — `byte_position` has already advanced
    /// past the data boundary by the time the decoder queries metadata.
    segment_position: Arc<AtomicU64>,

    // Seek coordinator fields (GStreamer FLUSH_START/STOP pattern)
    seek_epoch: Arc<AtomicU64>,
    seek_target_ns: Arc<AtomicU64>,

    /// Consolidated boolean state: `FLUSHING`, `SEEK_PENDING`, `EOF`, `PLAYING`.
    flags: Arc<AtomicU8>,
}

impl Timeline {
    const NO_PENDING_SEEK: u64 = u64::MAX;
    const NO_DURATION: u64 = u64::MAX;
    const NO_SEEK_TARGET: u64 = u64::MAX;

    #[must_use]
    pub fn new() -> Self {
        Self {
            byte_position: Arc::new(AtomicU64::new(0)),
            committed_position_ns: Arc::new(AtomicU64::new(0)),
            download_position: Arc::new(AtomicU64::new(0)),
            pending_seek_epoch: Arc::new(AtomicU64::new(Self::NO_PENDING_SEEK)),
            total_duration_ns: Arc::new(AtomicU64::new(Self::NO_DURATION)),
            segment_position: Arc::new(AtomicU64::new(0)),
            seek_epoch: Arc::new(AtomicU64::new(0)),
            seek_target_ns: Arc::new(AtomicU64::new(Self::NO_SEEK_TARGET)),
            flags: Arc::new(AtomicU8::new(TimelineFlags::empty().bits())),
        }
    }

    #[inline]
    fn flags_snapshot_with(&self, order: Ordering) -> TimelineFlags {
        TimelineFlags::from_bits_truncate(self.flags.load(order))
    }

    #[inline]
    fn insert_flags_with(&self, flags: TimelineFlags, order: Ordering) {
        self.flags.fetch_or(flags.bits(), order);
    }

    #[inline]
    fn remove_flags_with(&self, flags: TimelineFlags, order: Ordering) {
        self.flags.fetch_and(!flags.bits(), order);
    }

    #[inline]
    fn replace_flags(&self, flags: TimelineFlags, on: bool) {
        if on {
            self.insert_flags_with(flags, Ordering::Release);
        } else {
            self.remove_flags_with(flags, Ordering::Release);
        }
    }

    #[inline]
    fn contains_flag(&self, flag: TimelineFlags) -> bool {
        self.flags_snapshot_with(Ordering::Acquire).contains(flag)
    }

    #[must_use]
    pub fn byte_position(&self) -> u64 {
        self.byte_position.load(Ordering::Acquire)
    }

    pub fn set_byte_position(&self, position: u64) {
        self.byte_position.store(position, Ordering::Release);
    }

    #[must_use]
    pub fn segment_position(&self) -> u64 {
        self.segment_position.load(Ordering::Acquire)
    }

    pub fn set_segment_position(&self, position: u64) {
        self.segment_position.store(position, Ordering::Release);
    }

    #[must_use]
    pub fn download_position(&self) -> u64 {
        self.download_position.load(Ordering::Acquire)
    }

    pub fn set_download_position(&self, position: u64) {
        self.download_position.store(position, Ordering::Release);
    }

    #[must_use]
    pub fn committed_position(&self) -> Duration {
        Duration::from_nanos(self.committed_position_ns.load(Ordering::Acquire))
    }

    pub fn set_committed_position(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos()).unwrap_or(u64::MAX);
        self.committed_position_ns.store(nanos, Ordering::Release);
    }

    pub fn set_eof(&self, eof: bool) {
        self.replace_flags(TimelineFlags::EOF, eof);
    }

    #[must_use]
    pub fn eof(&self) -> bool {
        self.contains_flag(TimelineFlags::EOF)
    }

    pub fn set_total_duration(&self, duration: Option<Duration>) {
        let nanos = duration
            .and_then(|value| u64::try_from(value.as_nanos()).ok())
            .unwrap_or(Self::NO_DURATION);
        self.total_duration_ns.store(nanos, Ordering::Release);
    }

    #[must_use]
    pub fn total_duration(&self) -> Option<Duration> {
        let nanos = self.total_duration_ns.load(Ordering::Acquire);
        if nanos == Self::NO_DURATION {
            None
        } else {
            Some(Duration::from_nanos(nanos))
        }
    }

    pub fn advance_committed_samples(
        &self,
        interleaved_samples: u64,
        sample_rate: u32,
        channels: u16,
    ) {
        const NANOS_PER_SECOND: u128 = 1_000_000_000;

        if sample_rate == 0 || channels == 0 || interleaved_samples == 0 {
            return;
        }

        let frames = interleaved_samples / u64::from(channels);
        if frames == 0 {
            return;
        }
        let delta_nanos = (u128::from(frames) * NANOS_PER_SECOND) / u128::from(sample_rate);
        let delta = u64::try_from(delta_nanos).unwrap_or(u64::MAX);
        loop {
            let current = self.committed_position_ns.load(Ordering::Acquire);
            let next = current.saturating_add(delta);
            if self
                .committed_position_ns
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn mark_pending_seek_epoch(&self, seek_epoch: u64) {
        self.pending_seek_epoch.store(seek_epoch, Ordering::Release);
    }

    #[must_use]
    pub fn pending_seek_epoch(&self) -> Option<u64> {
        let epoch = self.pending_seek_epoch.load(Ordering::Acquire);
        if epoch == Self::NO_PENDING_SEEK {
            None
        } else {
            Some(epoch)
        }
    }

    #[must_use]
    pub fn clear_pending_seek_epoch(&self, seek_epoch: u64) -> bool {
        self.pending_seek_epoch
            .compare_exchange(
                seek_epoch,
                Self::NO_PENDING_SEEK,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Initiate a seek (`FLUSH_START`).
    ///
    /// Sets flushing flag, records target position, increments epoch.
    /// All blocking reads (`wait_range`) will observe `is_flushing()` and abort.
    ///
    /// Returns the new seek epoch.
    #[must_use]
    pub fn initiate_seek(&self, target: Duration) -> u64 {
        let nanos = u64::try_from(target.as_nanos()).unwrap_or(u64::MAX - 1);
        let epoch = self.seek_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        self.seek_target_ns.store(nanos, Ordering::Release);
        self.set_committed_position(target);
        self.insert_flags_with(TimelineFlags::SEEK_PENDING, Ordering::Release);
        // FLUSHING must be observed AFTER the seek target is published so
        // readers that see FLUSHING=true also see the updated target.
        self.insert_flags_with(TimelineFlags::FLUSHING, Ordering::Release);
        epoch
    }

    /// Complete a seek (`FLUSH_STOP`).
    ///
    /// Clears flushing flag only if `epoch` is still current.
    /// A superseding `initiate_seek` will have incremented the epoch,
    /// preventing an older completion from clearing the new seek.
    ///
    /// Uses a double-check to guard against the race where a new
    /// `initiate_seek` fires between our epoch load and flushing store.
    pub fn complete_seek(&self, epoch: u64) {
        if self.seek_epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        // NOTE: we do NOT clear seek_target_ns here.
        // A concurrent initiate_seek() may have already written a new target;
        // clearing it would lose that target. Stale targets are harmless
        // because apply_pending_seek() always gates on seek_epoch.
        self.remove_flags_with(TimelineFlags::FLUSHING, Ordering::SeqCst);
        // Double-check: if a newer seek arrived while we were clearing,
        // its initiate_seek may have already set FLUSHING=true which we
        // just cleared. Re-set FLUSHING to avoid losing the seek.
        if self.seek_epoch.load(Ordering::SeqCst) != epoch {
            self.insert_flags_with(TimelineFlags::FLUSHING, Ordering::SeqCst);
        }
    }

    /// Check if the pipeline is being flushed (seek pending).
    #[must_use]
    pub fn is_flushing(&self) -> bool {
        self.contains_flag(TimelineFlags::FLUSHING)
    }

    /// Read the pending seek target position.
    #[must_use]
    pub fn seek_target(&self) -> Option<Duration> {
        let ns = self.seek_target_ns.load(Ordering::Acquire);
        if ns == Self::NO_SEEK_TARGET {
            None
        } else {
            Some(Duration::from_nanos(ns))
        }
    }

    /// Read the current seek epoch.
    #[must_use]
    pub fn seek_epoch(&self) -> u64 {
        self.seek_epoch.load(Ordering::Acquire)
    }

    /// Check if a seek has been initiated but not yet applied by the decoder.
    ///
    /// Unlike `is_flushing()` (which gates I/O via `wait_range`), this flag
    /// stays set until the decoder successfully repositions. Used by the worker
    /// loop to trigger seek retry.
    #[must_use]
    pub fn is_seek_pending(&self) -> bool {
        self.contains_flag(TimelineFlags::SEEK_PENDING)
    }

    /// Clear seek-pending flag after the decoder successfully applied the seek.
    ///
    /// Only clears if `epoch` matches the current seek epoch, preventing a
    /// stale completion from clearing a newer seek.
    pub fn clear_seek_pending(&self, epoch: u64) {
        if self.seek_epoch.load(Ordering::Acquire) == epoch {
            self.remove_flags_with(TimelineFlags::SEEK_PENDING, Ordering::Release);
        }
    }

    /// Whether the audio FSM has claimed this Timeline as the currently
    /// active decode target.
    ///
    /// Written by the audio pipeline (`StreamAudioSource`) on entry into
    /// a decode-producing state and cleared on EOF / failure / unload.
    /// Read by the Downloader peer implementations to decide whether a
    /// track's fetches should be routed to the high-priority slot.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.contains_flag(TimelineFlags::PLAYING)
    }

    /// Toggle the `PLAYING` flag.
    ///
    /// Orthogonal to `FLUSHING` / `SEEK_PENDING` / `EOF`: toggling
    /// `PLAYING` does not affect the seek or EOF state.
    pub fn set_playing(&self, playing: bool) {
        self.replace_flags(TimelineFlags::PLAYING, playing);
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    #[kithara::test]
    fn download_position_defaults_to_zero() {
        let timeline = Timeline::new();
        assert_eq!(timeline.download_position(), 0);
    }

    #[kithara::test]
    fn download_position_is_shared_between_clones() {
        let timeline = Timeline::new();
        let clone = timeline.clone();

        timeline.set_download_position(512);
        assert_eq!(clone.download_position(), 512);
    }

    #[kithara::test]
    fn initiate_seek_sets_flushing_and_target() {
        let tl = Timeline::new();
        assert!(!tl.is_flushing());
        assert!(tl.seek_target().is_none());

        let epoch = tl.initiate_seek(Duration::from_secs(10));
        assert_eq!(epoch, 1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
        assert_eq!(tl.seek_epoch(), 1);
        assert_eq!(tl.committed_position(), Duration::from_secs(10));
    }

    #[kithara::test]
    fn complete_seek_clears_flushing() {
        let tl = Timeline::new();
        let epoch = tl.initiate_seek(Duration::from_secs(5));
        tl.complete_seek(epoch);
        assert!(!tl.is_flushing());
        // seek_target is intentionally NOT cleared — stale targets are
        // harmless because consumers gate on seek_epoch.
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(5)));
    }

    #[kithara::test]
    fn complete_seek_ignores_stale_epoch() {
        let tl = Timeline::new();
        let epoch1 = tl.initiate_seek(Duration::from_secs(5));
        let epoch2 = tl.initiate_seek(Duration::from_secs(10));
        tl.complete_seek(epoch1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
        tl.complete_seek(epoch2);
        assert!(!tl.is_flushing());
    }

    #[kithara::test]
    fn seek_epoch_monotonically_increases() {
        let tl = Timeline::new();
        let e1 = tl.initiate_seek(Duration::from_secs(1));
        let e2 = tl.initiate_seek(Duration::from_secs(2));
        let e3 = tl.initiate_seek(Duration::from_secs(3));
        assert_eq!(e1, 1);
        assert_eq!(e2, 2);
        assert_eq!(e3, 3);
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(3)));
    }

    #[kithara::test]
    fn complete_seek_does_not_clobber_concurrent_target() {
        let tl = Timeline::new();
        let epoch1 = tl.initiate_seek(Duration::from_secs(5));
        // Simulate concurrent initiate_seek before complete_seek runs
        let _epoch2 = tl.initiate_seek(Duration::from_secs(15));
        // complete_seek with stale epoch must not touch seek_target
        tl.complete_seek(epoch1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(15)));
    }

    #[kithara::test]
    fn initiate_seek_is_visible_across_clones() {
        let tl = Timeline::new();
        let clone = tl.clone();
        let _ = tl.initiate_seek(Duration::from_secs(7));
        assert!(clone.is_flushing());
        assert_eq!(clone.seek_target(), Some(Duration::from_secs(7)));
    }

    #[kithara::test]
    fn initiate_seek_sets_seek_pending() {
        let tl = Timeline::new();
        assert!(!tl.is_seek_pending());
        let _epoch = tl.initiate_seek(Duration::from_secs(5));
        assert!(tl.is_seek_pending());
    }

    #[kithara::test]
    fn clear_seek_pending_only_clears_matching_epoch() {
        let tl = Timeline::new();
        let epoch1 = tl.initiate_seek(Duration::from_secs(5));
        let epoch2 = tl.initiate_seek(Duration::from_secs(10));
        // Stale epoch should not clear seek_pending
        tl.clear_seek_pending(epoch1);
        assert!(tl.is_seek_pending());
        // Current epoch clears it
        tl.clear_seek_pending(epoch2);
        assert!(!tl.is_seek_pending());
    }

    #[kithara::test]
    fn new_initiate_seek_resets_seek_pending() {
        let tl = Timeline::new();
        let epoch = tl.initiate_seek(Duration::from_secs(5));
        tl.clear_seek_pending(epoch);
        assert!(!tl.is_seek_pending());
        // New seek re-sets seek_pending
        let _epoch2 = tl.initiate_seek(Duration::from_secs(10));
        assert!(tl.is_seek_pending());
    }

    #[kithara::test]
    fn complete_seek_does_not_clear_seek_pending() {
        let tl = Timeline::new();
        let epoch = tl.initiate_seek(Duration::from_secs(5));
        // complete_seek clears flushing but NOT seek_pending
        tl.complete_seek(epoch);
        assert!(!tl.is_flushing());
        assert!(tl.is_seek_pending());
    }

    #[kithara::test]
    fn is_seek_pending_visible_across_clones() {
        let tl = Timeline::new();
        let clone = tl.clone();
        let _epoch = tl.initiate_seek(Duration::from_secs(5));
        assert!(clone.is_seek_pending());
    }

    // --- bitflags migration tests ---

    #[kithara::test]
    fn set_eof_toggles_flag_without_affecting_others() {
        let tl = Timeline::new();
        assert!(!tl.eof());
        tl.set_eof(true);
        assert!(tl.eof());
        assert!(!tl.is_flushing());
        assert!(!tl.is_seek_pending());
        tl.set_eof(false);
        assert!(!tl.eof());
    }

    #[kithara::test]
    fn flag_triple_matrix_matches_bitflags_snapshot() {
        // Drive all 2^3 combinations of {flushing, seek_pending, eof} via
        // the public API and confirm flags_snapshot_with() reports them.
        for mask in 0u8..8 {
            let tl = Timeline::new();
            let want_flushing = mask & 1 != 0;
            let want_seek_pending = mask & 2 != 0;
            let want_eof = mask & 4 != 0;

            if want_flushing || want_seek_pending {
                let _ = tl.initiate_seek(Duration::from_secs(1));
                if !want_flushing {
                    tl.complete_seek(tl.seek_epoch());
                }
                if !want_seek_pending {
                    tl.clear_seek_pending(tl.seek_epoch());
                }
            }
            tl.set_eof(want_eof);

            assert_eq!(tl.is_flushing(), want_flushing, "mask {mask:#05b} flushing");
            assert_eq!(
                tl.is_seek_pending(),
                want_seek_pending,
                "mask {mask:#05b} seek_pending"
            );
            assert_eq!(tl.eof(), want_eof, "mask {mask:#05b} eof");

            let snapshot = tl.flags_snapshot_with(Ordering::Acquire);
            assert_eq!(
                snapshot.contains(TimelineFlags::FLUSHING),
                want_flushing,
                "mask {mask:#05b} snapshot flushing"
            );
            assert_eq!(
                snapshot.contains(TimelineFlags::SEEK_PENDING),
                want_seek_pending,
                "mask {mask:#05b} snapshot seek_pending"
            );
            assert_eq!(
                snapshot.contains(TimelineFlags::EOF),
                want_eof,
                "mask {mask:#05b} snapshot eof"
            );
        }
    }

    #[kithara::test]
    fn complete_seek_double_check_re_raises_flushing_when_newer_seek_interleaves() {
        // Simulate the race: complete_seek runs for epoch1; between
        // clearing FLUSHING and the double-check, a newer initiate_seek
        // fires and bumps the epoch. The double-check must observe the
        // epoch mismatch and re-set FLUSHING.
        let tl = Timeline::new();
        let epoch1 = tl.initiate_seek(Duration::from_secs(1));

        // Simulate the interleave by hand: clear FLUSHING as
        // complete_seek's first store would, then bump the epoch via
        // initiate_seek before re-entering complete_seek.
        tl.remove_flags_with(TimelineFlags::FLUSHING, Ordering::SeqCst);
        let _epoch2 = tl.initiate_seek(Duration::from_secs(2));
        tl.complete_seek(epoch1);

        assert!(
            tl.is_flushing(),
            "FLUSHING must be re-raised when a newer seek interleaves mid-complete"
        );
    }

    #[kithara::test]
    fn concurrent_flag_toggles_preserve_independent_semantics() {
        // Four threads toggle disjoint flags in a tight loop. Asserts no
        // thread's operation clobbers another flag — the OR/AND-based
        // primitives compose correctly.
        const ITER: usize = 50_000;

        let tl = Timeline::new();
        let barrier = Arc::new(Barrier::new(3));

        // Thread A: flips EOF on/off.
        let tl_a = tl.clone();
        let barrier_a = Arc::clone(&barrier);
        let a = thread::spawn(move || {
            barrier_a.wait();
            for i in 0..ITER {
                tl_a.set_eof(i % 2 == 0);
            }
        });

        // Thread B: repeatedly sets/clears SEEK_PENDING via public API.
        let tl_b = tl.clone();
        let barrier_b = Arc::clone(&barrier);
        let b = thread::spawn(move || {
            barrier_b.wait();
            for _ in 0..ITER {
                let epoch = tl_b.initiate_seek(Duration::from_millis(1));
                tl_b.clear_seek_pending(epoch);
                tl_b.complete_seek(epoch);
            }
        });

        // Thread C: observes snapshots without crashing.
        let tl_c = tl.clone();
        let barrier_c = Arc::clone(&barrier);
        let c = thread::spawn(move || {
            barrier_c.wait();
            let mut observed = 0u64;
            for _ in 0..ITER {
                let snap = tl_c.flags_snapshot_with(Ordering::Acquire);
                observed ^= u64::from(snap.bits());
            }
            observed
        });

        a.join().expect("thread A");
        b.join().expect("thread B");
        let _ = c.join().expect("thread C");

        // Final invariant: after all writers finish, EOF reflects the
        // last A iteration (ITER-1 ⇒ odd ⇒ false); FLUSHING/SEEK_PENDING
        // fully cleared by B's last complete_seek + clear_seek_pending.
        assert!(!tl.eof(), "EOF must match the last deterministic write");
        assert!(!tl.is_flushing(), "FLUSHING must be fully cleared");
        assert!(
            !tl.is_seek_pending(),
            "SEEK_PENDING must be fully cleared after last clear"
        );
    }

    // --- PLAYING flag tests (Task 4) ---

    #[kithara::test]
    fn playing_defaults_to_false() {
        let tl = Timeline::new();
        assert!(!tl.is_playing());
    }

    #[kithara::test]
    fn set_playing_true_is_visible_across_clones() {
        let tl = Timeline::new();
        let clone = tl.clone();
        tl.set_playing(true);
        assert!(clone.is_playing());
        clone.set_playing(false);
        assert!(!tl.is_playing());
    }

    #[kithara::test]
    fn set_playing_idempotent() {
        let tl = Timeline::new();
        tl.set_playing(true);
        tl.set_playing(true);
        assert!(tl.is_playing());
        tl.set_playing(false);
        tl.set_playing(false);
        assert!(!tl.is_playing());
    }

    #[kithara::test]
    fn playing_is_orthogonal_to_other_flags() {
        // Test all 8 combinations of {flushing, seek_pending, eof} paired
        // with PLAYING=0 and PLAYING=1. Assert PLAYING mirror the writer
        // and the other flags are untouched by set_playing.
        for mask in 0u8..8 {
            for &initial_playing in &[false, true] {
                let tl = Timeline::new();
                let want_flushing = mask & 1 != 0;
                let want_seek_pending = mask & 2 != 0;
                let want_eof = mask & 4 != 0;

                if want_flushing || want_seek_pending {
                    let _ = tl.initiate_seek(Duration::from_secs(1));
                    if !want_flushing {
                        tl.complete_seek(tl.seek_epoch());
                    }
                    if !want_seek_pending {
                        tl.clear_seek_pending(tl.seek_epoch());
                    }
                }
                tl.set_eof(want_eof);
                tl.set_playing(initial_playing);

                assert_eq!(tl.is_playing(), initial_playing);
                assert_eq!(
                    tl.is_flushing(),
                    want_flushing,
                    "mask {mask:#05b} play={initial_playing} flushing"
                );
                assert_eq!(
                    tl.is_seek_pending(),
                    want_seek_pending,
                    "mask {mask:#05b} play={initial_playing} seek_pending"
                );
                assert_eq!(
                    tl.eof(),
                    want_eof,
                    "mask {mask:#05b} play={initial_playing} eof"
                );

                // Now toggle PLAYING and assert the other three flags
                // remain exactly as set.
                tl.set_playing(!initial_playing);
                assert_eq!(tl.is_playing(), !initial_playing);
                assert_eq!(tl.is_flushing(), want_flushing);
                assert_eq!(tl.is_seek_pending(), want_seek_pending);
                assert_eq!(tl.eof(), want_eof);
            }
        }
    }

    #[kithara::test]
    fn initiate_seek_does_not_touch_playing() {
        let tl = Timeline::new();
        tl.set_playing(true);
        let epoch = tl.initiate_seek(Duration::from_secs(5));
        assert!(tl.is_playing(), "PLAYING must not be affected by seek");
        tl.complete_seek(epoch);
        assert!(tl.is_playing(), "PLAYING must survive complete_seek");
        tl.clear_seek_pending(epoch);
        assert!(tl.is_playing(), "PLAYING must survive clear_seek_pending");
    }
}
