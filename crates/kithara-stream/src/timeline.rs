#![forbid(unsafe_code)]

#[cfg(test)]
use std::sync::atomic::Ordering;
use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use bitflags::bitflags;
use kithara_test_utils::kithara;

use crate::{
    playhead::{PlayheadRead, PlayheadState, PlayheadWrite},
    seek_state::{Activity, SeekControl, SeekObserve, SeekState},
};

/// Decoder-reported chunk position used to advance the timeline.
///
/// This struct is the kithara-stream-local mirror of the fields
/// [`PlayheadWrite::advance`] needs from a decoder's
/// per-chunk metadata. It exists because `PcmMeta` lives in
/// `kithara-decode` (which depends on `kithara-stream`); a tiny mirror
/// avoids the circular dep without forcing decoders to fragment their
/// existing meta type.
///
/// Decoder backends fill it from their own meta — see
/// `From<&PcmMeta> for ChunkPosition` in `kithara-decode`.
#[derive(Debug, Clone, Copy)]
pub struct ChunkPosition {
    /// Absolute byte offset of the chunk's source data when the
    /// decoder reports it (Apple `mStartOffset`, Android API 28+).
    pub source_byte_offset: Option<u64>,
    /// Decoder-reported wall-clock position **after** the chunk has
    /// been emitted (or, for [`Timeline::commit_seek_landed`], the
    /// landed position). Authoritative — derived from the decoder's
    /// own frame counter inside its own arithmetic, so the timeline
    /// never recomputes `frames * 1e9 / sample_rate`. Always strictly
    /// greater than (or equal to, for seek landings) the chunk start.
    pub end_position_ns: u64,
    /// Absolute frame offset of the *first* frame in the chunk.
    pub frame_offset: u64,
    /// Number of frames the chunk covers.
    pub frames: u64,
    /// Source bytes the chunk decoded from (decoder ground truth).
    pub source_bytes: u64,
}

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
    }
}

/// Shared playback timeline used across stream layers.
///
/// Stores canonical committed playback position. The byte cursor lives
/// on the [`Source`](crate::Source) — sources own per-variant or
/// per-file atomic cursors and expose them through
/// [`Source::position`](crate::Source::position) /
/// [`Source::advance`](crate::Source::advance) /
/// [`Source::set_position`](crate::Source::set_position).
#[derive(Clone, Debug)]
pub struct Timeline {
    playhead: Arc<PlayheadState>,
    seek: Arc<SeekState>,
}

impl Timeline {
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self {
            playhead: Arc::new(PlayheadState::new()),
            seek: Arc::new(SeekState::new()),
        }
    }

    /// Vend a clone of the inner `Arc<PlayheadState>` — used by `Source`
    /// default methods to produce typed trait-object handles.
    pub(crate) fn playhead_arc(&self) -> Arc<PlayheadState> {
        Arc::clone(&self.playhead)
    }

    /// Vend a clone of the inner `Arc<SeekState>` — used by `Source`
    /// default methods to produce typed trait-object handles.
    pub(crate) fn seek_arc(&self) -> Arc<SeekState> {
        Arc::clone(&self.seek)
    }

    /// Narrow mutating playhead handle vended as a trait object.
    #[must_use]
    pub fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    /// Narrow seek-control handle vended as a trait object — begin / complete / `mark_pending`.
    #[must_use]
    pub fn seek_control(&self) -> Arc<dyn SeekControl> {
        Arc::clone(&self.seek) as Arc<dyn SeekControl>
    }

    /// Narrow seek-observe handle vended as a trait object.
    #[must_use]
    pub fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    /// Narrow read-only playhead handle vended as a trait object.
    #[must_use]
    pub fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    /// Narrow activity handle vended as a trait object — `is_playing` / `set_playing`.
    #[must_use]
    pub fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    #[must_use]
    pub fn committed_position(&self) -> Duration {
        self.playhead.position()
    }

    /// Read the raw flags snapshot (used in tests and the concurrent flag test).
    #[cfg(test)]
    pub(crate) fn flags_snapshot_with(&self, order: Ordering) -> TimelineFlags {
        TimelineFlags::from_bits_truncate(self.seek.flags_raw(order))
    }

    /// Clear specific flags — test-only helper used to simulate concurrent
    /// interleaved operations (e.g. the `complete_seek` double-check test).
    #[cfg(test)]
    pub(crate) fn remove_flags_with(&self, flags: TimelineFlags, order: Ordering) {
        self.seek.remove_flags_raw(flags, order);
    }

    /// Check if the pipeline is being flushed (seek pending).
    #[must_use]
    pub fn is_flushing(&self) -> bool {
        self.seek.is_flushing()
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
        self.seek.is_playing()
    }

    /// Read the current seek epoch.
    #[must_use]
    pub fn seek_epoch(&self) -> u64 {
        self.seek.epoch()
    }

    /// Cheap clone of the shared atomic seek epoch — same use case.
    #[must_use]
    pub fn seek_epoch_handle(&self) -> Arc<AtomicU64> {
        self.seek.seek_epoch_arc()
    }

    /// Read the pending seek target position.
    #[must_use]
    pub fn seek_target(&self) -> Option<Duration> {
        self.seek.target()
    }

    /// Report the current download byte position. The value is not
    /// stored on the timeline — it exists only as a USDT probe point
    /// (`#[kithara::probe]`) for download-progress observability.
    #[kithara::probe(position)]
    pub fn set_download_position(&self, position: u64) {
        let _ = position;
    }

    /// Toggle the `PLAYING` flag.
    ///
    /// Orthogonal to `FLUSHING` / `SEEK_PENDING`: toggling `PLAYING`
    /// does not affect the seek state.
    pub fn set_playing(&self, playing: bool) {
        self.seek.set_playing(playing);
    }

    pub fn set_total_duration(&self, duration: Option<Duration>) {
        self.playhead.set_duration(duration);
    }

    #[must_use]
    pub fn total_duration(&self) -> Option<Duration> {
        self.playhead.duration()
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
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

    #[kithara::test]
    fn initiate_seek_sets_flushing_and_target() {
        let tl = Timeline::new();
        assert!(!tl.is_flushing());
        assert!(tl.seek_target().is_none());
        let initial_committed = tl.committed_position();

        let epoch = tl.seek_control().begin(Duration::from_secs(10));
        assert_eq!(epoch, 1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
        assert_eq!(tl.seek_epoch(), 1);
        assert_eq!(tl.committed_position(), initial_committed);
    }

    #[kithara::test]
    fn complete_seek_clears_flushing() {
        let tl = Timeline::new();
        let epoch = tl.seek_control().begin(Duration::from_secs(5));
        tl.seek_control().complete(epoch);
        assert!(!tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(5)));
    }

    #[kithara::test]
    fn complete_seek_ignores_stale_epoch() {
        let tl = Timeline::new();
        let epoch1 = tl.seek_control().begin(Duration::from_secs(5));
        let epoch2 = tl.seek_control().begin(Duration::from_secs(10));
        tl.seek_control().complete(epoch1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
        tl.seek_control().complete(epoch2);
        assert!(!tl.is_flushing());
    }

    #[kithara::test]
    fn seek_epoch_monotonically_increases() {
        let tl = Timeline::new();
        let e1 = tl.seek_control().begin(Duration::from_secs(1));
        let e2 = tl.seek_control().begin(Duration::from_secs(2));
        let e3 = tl.seek_control().begin(Duration::from_secs(3));
        assert_eq!(e1, 1);
        assert_eq!(e2, 2);
        assert_eq!(e3, 3);
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(3)));
    }

    #[kithara::test]
    fn complete_seek_does_not_clobber_concurrent_target() {
        let tl = Timeline::new();
        let epoch1 = tl.seek_control().begin(Duration::from_secs(5));
        let _epoch2 = tl.seek_control().begin(Duration::from_secs(15));
        tl.seek_control().complete(epoch1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(15)));
    }

    #[kithara::test]
    fn initiate_seek_is_visible_across_clones() {
        let tl = Timeline::new();
        let clone = tl.clone();
        let _ = tl.seek_control().begin(Duration::from_secs(7));
        assert!(clone.is_flushing());
        assert_eq!(clone.seek_target(), Some(Duration::from_secs(7)));
    }

    #[kithara::test]
    fn initiate_seek_sets_seek_pending() {
        let tl = Timeline::new();
        assert!(!tl.seek_observe().is_pending());
        let _epoch = tl.seek_control().begin(Duration::from_secs(5));
        assert!(tl.seek_observe().is_pending());
    }

    #[kithara::test]
    fn clear_seek_pending_only_clears_matching_epoch() {
        let tl = Timeline::new();
        let epoch1 = tl.seek_control().begin(Duration::from_secs(5));
        let epoch2 = tl.seek_control().begin(Duration::from_secs(10));
        tl.seek_control().clear_pending(epoch1);
        assert!(tl.seek_observe().is_pending());
        tl.seek_control().clear_pending(epoch2);
        assert!(!tl.seek_observe().is_pending());
    }

    #[kithara::test]
    fn new_initiate_seek_resets_seek_pending() {
        let tl = Timeline::new();
        let epoch = tl.seek_control().begin(Duration::from_secs(5));
        tl.seek_control().clear_pending(epoch);
        assert!(!tl.seek_observe().is_pending());
        let _epoch2 = tl.seek_control().begin(Duration::from_secs(10));
        assert!(tl.seek_observe().is_pending());
    }

    #[kithara::test]
    fn complete_seek_does_not_clear_seek_pending() {
        let tl = Timeline::new();
        let epoch = tl.seek_control().begin(Duration::from_secs(5));
        tl.seek_control().complete(epoch);
        assert!(!tl.is_flushing());
        assert!(tl.seek_observe().is_pending());
    }

    #[kithara::test]
    fn is_seek_pending_visible_across_clones() {
        let tl = Timeline::new();
        let clone = tl.clone();
        let _epoch = tl.seek_control().begin(Duration::from_secs(5));
        assert!(clone.seek_observe().is_pending());
    }

    #[kithara::test]
    fn flag_pair_matrix_matches_bitflags_snapshot() {
        for mask in 0u8..4 {
            let tl = Timeline::new();
            let want_flushing = mask & 1 != 0;
            let want_seek_pending = mask & 2 != 0;

            if want_flushing || want_seek_pending {
                let sc = tl.seek_control();
                let _ = sc.begin(Duration::from_secs(1));
                if !want_flushing {
                    sc.complete(tl.seek_epoch());
                }
                if !want_seek_pending {
                    sc.clear_pending(tl.seek_epoch());
                }
            }

            assert_eq!(tl.is_flushing(), want_flushing, "mask {mask:#04b} flushing");
            assert_eq!(
                tl.seek_observe().is_pending(),
                want_seek_pending,
                "mask {mask:#04b} seek_pending"
            );

            let snapshot = tl.flags_snapshot_with(Ordering::Acquire);
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
        let tl = Timeline::new();
        let epoch1 = tl.seek_control().begin(Duration::from_secs(1));

        tl.remove_flags_with(TimelineFlags::FLUSHING, Ordering::SeqCst);
        let _epoch2 = tl.seek_control().begin(Duration::from_secs(2));
        tl.seek_control().complete(epoch1);

        assert!(
            tl.is_flushing(),
            "FLUSHING must be re-raised when a newer seek interleaves mid-complete"
        );
    }

    #[kithara::test]
    fn concurrent_flag_toggles_preserve_independent_semantics() {
        const ITER: usize = 50_000;

        let tl = Timeline::new();
        let barrier = Arc::new(Barrier::new(3));

        let tl_a = tl.clone();
        let barrier_a = Arc::clone(&barrier);
        let a = thread::spawn(move || {
            barrier_a.wait();
            for i in 0..ITER {
                tl_a.set_playing(i % 2 == 0);
            }
        });

        let tl_b = tl.clone();
        let barrier_b = Arc::clone(&barrier);
        let b = thread::spawn(move || {
            barrier_b.wait();
            for _ in 0..ITER {
                let sc = tl_b.seek_control();
                let epoch = sc.begin(Duration::from_millis(1));
                sc.clear_pending(epoch);
                sc.complete(epoch);
            }
        });

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

        a.join()
            .expect("BUG: spawned thread A must not panic in this test");
        b.join()
            .expect("BUG: spawned thread B must not panic in this test");
        let _ = c
            .join()
            .expect("BUG: spawned thread C must not panic in this test");

        assert!(
            !tl.is_playing(),
            "PLAYING must match the last deterministic write"
        );
        assert!(!tl.is_flushing(), "FLUSHING must be fully cleared");
        assert!(
            !tl.seek_observe().is_pending(),
            "SEEK_PENDING must be fully cleared after last clear"
        );
    }

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
        for mask in 0u8..4 {
            for &initial_playing in &[false, true] {
                let tl = Timeline::new();
                let want_flushing = mask & 1 != 0;
                let want_seek_pending = mask & 2 != 0;

                if want_flushing || want_seek_pending {
                    let sc = tl.seek_control();
                    let _ = sc.begin(Duration::from_secs(1));
                    if !want_flushing {
                        sc.complete(tl.seek_epoch());
                    }
                    if !want_seek_pending {
                        sc.clear_pending(tl.seek_epoch());
                    }
                }
                tl.set_playing(initial_playing);

                assert_eq!(tl.is_playing(), initial_playing);
                assert_eq!(
                    tl.is_flushing(),
                    want_flushing,
                    "mask {mask:#04b} play={initial_playing} flushing"
                );
                assert_eq!(
                    tl.seek_observe().is_pending(),
                    want_seek_pending,
                    "mask {mask:#04b} play={initial_playing} seek_pending"
                );

                tl.set_playing(!initial_playing);
                assert_eq!(tl.is_playing(), !initial_playing);
                assert_eq!(tl.is_flushing(), want_flushing);
                assert_eq!(tl.seek_observe().is_pending(), want_seek_pending);
            }
        }
    }

    #[kithara::test]
    fn advance_committed_chunk_updates_position_and_caps_at_duration() {
        let tl = Timeline::new();
        tl.set_total_duration(Some(Duration::from_secs(10)));

        let pos = ChunkPosition {
            frame_offset: 0,
            frames: 44100,
            end_position_ns: 1_000_000_000,
            source_bytes: 4096,
            source_byte_offset: None,
        };
        tl.playhead_write().advance(&pos);
        assert_eq!(
            tl.committed_position(),
            Duration::from_nanos(1_000_000_000),
            "committed_position must reflect end_position_ns"
        );

        // Value past total_duration is capped.
        let pos_past = ChunkPosition {
            frame_offset: 0,
            frames: 44100,
            end_position_ns: 15_000_000_000,
            source_bytes: 4096,
            source_byte_offset: None,
        };
        tl.playhead_write().advance(&pos_past);
        assert_eq!(
            tl.committed_position(),
            Duration::from_secs(10),
            "committed_position must be capped at total_duration"
        );
    }

    #[kithara::test]
    fn initiate_seek_does_not_touch_playing() {
        let tl = Timeline::new();
        tl.set_playing(true);
        let sc = tl.seek_control();
        let epoch = sc.begin(Duration::from_secs(5));
        assert!(tl.is_playing(), "PLAYING must not be affected by seek");
        sc.complete(epoch);
        assert!(tl.is_playing(), "PLAYING must survive complete_seek");
        sc.clear_pending(epoch);
        assert!(tl.is_playing(), "PLAYING must survive clear_seek_pending");
    }
}
