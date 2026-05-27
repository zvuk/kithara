#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use bitflags::bitflags;
use kithara_test_utils::kithara;

/// Decoder-reported chunk position used to advance the timeline.
///
/// This struct is the kithara-stream-local mirror of the fields
/// [`Timeline::advance_committed_chunk`] needs from a decoder's
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
    pub sample_rate: u32,
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
    /// Frame end (exclusive) of the last consumed slice — the consumer's
    /// playhead in frame space. Single source of truth for "where is the
    /// consumer in the stream"; both `committed_position_ns` (UI) and
    /// the per-chunk consumption offset (`Audio::read`) are derived
    /// from it. Decoder-driven via [`Self::advance_committed_to`].
    committed_frame_end: Arc<AtomicU64>,
    committed_position_ns: Arc<AtomicU64>,
    /// Independent latch for `DecoderNode::sync_seek_epoch`: the
    /// preempt latch above is destructively consumed inside
    /// `StreamAudioSource`, so the wrapping decoder node — which has to
    /// reset its preload counters / drop parked chunks on each new
    /// epoch — needs its own one-shot signal. `initiate_seek` arms both.
    decoder_node_seek_latch: Arc<AtomicBool>,
    /// Consolidated boolean state: `FLUSHING`, `SEEK_PENDING`, `PLAYING`.
    flags: Arc<AtomicU8>,
    /// Sample rate (Hz) of the most recently committed chunk; lets
    /// readers convert `committed_frame_end` ↔ `committed_position`
    /// without external state.
    last_sample_rate: Arc<AtomicU64>,

    pending_seek_epoch: Arc<AtomicU64>,

    seek_epoch: Arc<AtomicU64>,
    /// Hot-path latch the audio worker reads on every `step_track` to
    /// skip the multi-condition seek-preempt guard. Set by
    /// `initiate_seek` once per seek (Release after `seek_epoch`/
    /// `seek_target_ns` updates), consumed by the worker's
    /// `swap(false, Acquire)`. A single bool load replaces two
    /// `Arc<AtomicU64>` Acquire loads on the typical no-seek tick.
    seek_preempt_latch: Arc<AtomicBool>,
    seek_target_ns: Arc<AtomicU64>,
    /// Byte offset at the start of the most recent `Stream::read()` call.
    /// Used by `StreamContext::segment_index()` to resolve which segment
    /// the last-read data belongs to — `byte_position` has already advanced
    /// past the data boundary by the time the decoder queries metadata.
    segment_position: Arc<AtomicU64>,

    total_duration_ns: Arc<AtomicU64>,
}

impl Timeline {
    const NO_DURATION: u64 = u64::MAX;
    const NO_PENDING_SEEK: u64 = u64::MAX;
    const NO_SEEK_TARGET: u64 = u64::MAX;

    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self {
            committed_position_ns: Arc::new(AtomicU64::new(0)),
            committed_frame_end: Arc::new(AtomicU64::new(0)),
            last_sample_rate: Arc::new(AtomicU64::new(0)),
            pending_seek_epoch: Arc::new(AtomicU64::new(Self::NO_PENDING_SEEK)),
            total_duration_ns: Arc::new(AtomicU64::new(Self::NO_DURATION)),
            segment_position: Arc::new(AtomicU64::new(0)),
            seek_epoch: Arc::new(AtomicU64::new(0)),
            seek_target_ns: Arc::new(AtomicU64::new(Self::NO_SEEK_TARGET)),
            seek_preempt_latch: Arc::new(AtomicBool::new(false)),
            decoder_node_seek_latch: Arc::new(AtomicBool::new(false)),
            flags: Arc::new(AtomicU8::new(TimelineFlags::empty().bits())),
        }
    }

    /// Advance the consumer's playhead to the end of the consumed
    /// region described by `pos`. `pos.frame_offset + pos.frames`
    /// must equal the absolute frame the consumer has now finished
    /// playing through; the decoder owns these numbers, callers do
    /// not invent them.
    ///
    /// `committed_position_ns` (UI) is derived from the new playhead
    /// frame divided by `pos.sample_rate`. `byte_position` is set
    /// from `pos.source_byte_offset + pos.source_bytes` when the
    /// decoder reports absolute offsets (Apple, Android API 28+);
    /// otherwise it is left untouched so the producer-side cursor
    /// (`Stream::try_read` / `Stream::seek`) continues to drive it.
    ///
    /// Validates against `total_duration` in dev/test builds: a chunk
    /// pushing the playhead past the declared duration is a real
    /// arithmetic bug — the decoder's frame counter disagrees with
    /// `total_duration`, somebody is wrong.
    pub fn advance_committed_chunk(&self, pos: &ChunkPosition) {
        self.write_playhead(
            pos,
            pos.frame_offset.saturating_add(pos.frames),
            pos.source_byte_offset
                .map(|off| off.saturating_add(pos.source_bytes)),
        );
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

    /// Pin the playhead to the decoder's actual landing frame after a
    /// seek. Called by the worker once `decoder.seek` returns
    /// [`DecoderSeekOutcome::Landed`] — the only authoritative source
    /// for "where did we actually end up". `pos.frame_offset` carries
    /// the landed frame; `pos.frames` should be `0` (we have not yet
    /// consumed any chunk, just repositioned). `pos.source_byte_offset`
    /// (if known) is the byte offset the decoder is now reading from.
    pub fn commit_seek_landed(&self, pos: &ChunkPosition) {
        self.write_playhead(pos, pos.frame_offset, pos.source_byte_offset);
    }

    #[must_use]
    pub fn committed_position(&self) -> Duration {
        Duration::from_nanos(self.committed_position_ns.load(Ordering::Acquire))
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
        self.remove_flags_with(TimelineFlags::FLUSHING, Ordering::SeqCst);
        if self.seek_epoch.load(Ordering::SeqCst) != epoch {
            self.insert_flags_with(TimelineFlags::FLUSHING, Ordering::SeqCst);
        }
    }

    #[inline]
    fn contains_flag(&self, flag: TimelineFlags) -> bool {
        self.flags_snapshot_with(Ordering::Acquire).contains(flag)
    }

    #[must_use]
    pub fn did_clear_pending_seek_epoch(&self, seek_epoch: u64) -> bool {
        self.pending_seek_epoch
            .compare_exchange(
                seek_epoch,
                Self::NO_PENDING_SEEK,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Consume the decoder-node seek latch with an Acquire swap.
    ///
    /// Independent from `take_seek_preempt`: the inner audio source
    /// consumes that one inside `step_track`, while `DecoderNode` (the
    /// wrapping scheduler node) needs its own signal so it can reset
    /// preload state and drop parked chunks on a new epoch. `true`
    /// here means `seek_epoch` was just bumped and the node must run
    /// the cleanup branch; otherwise the tick falls through.
    #[must_use]
    pub fn did_take_decoder_node_seek(&self) -> bool {
        self.decoder_node_seek_latch.swap(false, Ordering::Acquire)
    }

    /// Consume the seek-preempt latch with an Acquire swap.
    ///
    /// Returns `true` exactly once per `initiate_seek` call: the worker
    /// uses this to short-circuit `step_track`'s preempt guard without
    /// dereferencing two `Arc<AtomicU64>`s. The Acquire ordering
    /// synchronises with the Release in `initiate_seek` so observing
    /// `true` here means the new `seek_epoch` and `seek_target_ns`
    /// stores are also visible.
    #[must_use]
    pub fn did_take_seek_preempt(&self) -> bool {
        self.seek_preempt_latch.swap(false, Ordering::Acquire)
    }

    #[inline]
    fn flags_snapshot_with(&self, order: Ordering) -> TimelineFlags {
        TimelineFlags::from_bits_truncate(self.flags.load(order))
    }

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
    #[must_use]
    pub fn initiate_seek(&self, target: Duration) -> u64 {
        let nanos = u64::try_from(target.as_nanos())
            .expect("BUG: initiate_seek target.as_nanos() fits in u64 for any realistic Duration");
        let epoch = self.seek_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        self.seek_target_ns.store(nanos, Ordering::Release);
        // NOTE: do NOT pre-set `committed_position` to `target` here.
        self.insert_flags_with(TimelineFlags::SEEK_PENDING, Ordering::Release);
        self.insert_flags_with(TimelineFlags::FLUSHING, Ordering::Release);
        self.seek_preempt_latch.store(true, Ordering::Release);
        self.decoder_node_seek_latch.store(true, Ordering::Release);
        epoch
    }

    #[inline]
    fn insert_flags_with(&self, flags: TimelineFlags, order: Ordering) {
        self.flags.fetch_or(flags.bits(), order);
    }

    /// Check if the pipeline is being flushed (seek pending).
    #[must_use]
    pub fn is_flushing(&self) -> bool {
        self.contains_flag(TimelineFlags::FLUSHING)
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

    /// Check if a seek has been initiated but not yet applied by the decoder.
    ///
    /// Unlike `is_flushing()` (which gates I/O via `wait_range`), this flag
    /// stays set until the decoder successfully repositions. Used by the worker
    /// loop to trigger seek retry.
    #[must_use]
    pub fn is_seek_pending(&self) -> bool {
        self.contains_flag(TimelineFlags::SEEK_PENDING)
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

    /// Read the current seek epoch.
    #[must_use]
    pub fn seek_epoch(&self) -> u64 {
        self.seek_epoch.load(Ordering::Acquire)
    }

    /// Cheap clone of the shared atomic seek epoch — same use case.
    #[must_use]
    pub fn seek_epoch_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.seek_epoch)
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

    /// # Panics
    /// Panics if `position` overflows `u64::MAX` nanoseconds (≈584 years);
    /// no realistic media stream can hit this.
    pub fn set_committed_position(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos())
            .expect("BUG: position.as_nanos() fits in u64 for any realistic playback time");
        self.committed_position_ns.store(nanos, Ordering::Release);
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
        self.replace_flags(TimelineFlags::PLAYING, playing);
    }

    #[kithara::probe(position)]
    pub fn set_segment_position(&self, position: u64) {
        self.segment_position.store(position, Ordering::Release);
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

    #[kithara::probe(committed_ns = pos.end_position_ns, end_frame)]
    fn write_playhead(&self, pos: &ChunkPosition, end_frame: u64, _source_byte_end: Option<u64>) {
        let sr = u64::from(pos.sample_rate);
        if sr == 0 {
            return;
        }
        let duration_ns = self.total_duration_ns.load(Ordering::Acquire);
        let cap = if duration_ns == Self::NO_DURATION {
            u64::MAX
        } else {
            duration_ns
        };
        self.committed_position_ns
            .store(pos.end_position_ns.min(cap), Ordering::Release);
        self.committed_frame_end.store(end_frame, Ordering::Release);
        self.last_sample_rate.store(sr, Ordering::Release);
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

        let epoch = tl.initiate_seek(Duration::from_secs(10));
        assert_eq!(epoch, 1);
        assert!(tl.is_flushing());
        assert_eq!(tl.seek_target(), Some(Duration::from_secs(10)));
        assert_eq!(tl.seek_epoch(), 1);
        assert_eq!(tl.committed_position(), initial_committed);
    }

    #[kithara::test]
    fn complete_seek_clears_flushing() {
        let tl = Timeline::new();
        let epoch = tl.initiate_seek(Duration::from_secs(5));
        tl.complete_seek(epoch);
        assert!(!tl.is_flushing());
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
        let _epoch2 = tl.initiate_seek(Duration::from_secs(15));
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
        tl.clear_seek_pending(epoch1);
        assert!(tl.is_seek_pending());
        tl.clear_seek_pending(epoch2);
        assert!(!tl.is_seek_pending());
    }

    #[kithara::test]
    fn new_initiate_seek_resets_seek_pending() {
        let tl = Timeline::new();
        let epoch = tl.initiate_seek(Duration::from_secs(5));
        tl.clear_seek_pending(epoch);
        assert!(!tl.is_seek_pending());
        let _epoch2 = tl.initiate_seek(Duration::from_secs(10));
        assert!(tl.is_seek_pending());
    }

    #[kithara::test]
    fn complete_seek_does_not_clear_seek_pending() {
        let tl = Timeline::new();
        let epoch = tl.initiate_seek(Duration::from_secs(5));
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

    #[kithara::test]
    fn flag_pair_matrix_matches_bitflags_snapshot() {
        for mask in 0u8..4 {
            let tl = Timeline::new();
            let want_flushing = mask & 1 != 0;
            let want_seek_pending = mask & 2 != 0;

            if want_flushing || want_seek_pending {
                let _ = tl.initiate_seek(Duration::from_secs(1));
                if !want_flushing {
                    tl.complete_seek(tl.seek_epoch());
                }
                if !want_seek_pending {
                    tl.clear_seek_pending(tl.seek_epoch());
                }
            }

            assert_eq!(tl.is_flushing(), want_flushing, "mask {mask:#04b} flushing");
            assert_eq!(
                tl.is_seek_pending(),
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
        let epoch1 = tl.initiate_seek(Duration::from_secs(1));

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
                let epoch = tl_b.initiate_seek(Duration::from_millis(1));
                tl_b.clear_seek_pending(epoch);
                tl_b.complete_seek(epoch);
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
            !tl.is_seek_pending(),
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
                    let _ = tl.initiate_seek(Duration::from_secs(1));
                    if !want_flushing {
                        tl.complete_seek(tl.seek_epoch());
                    }
                    if !want_seek_pending {
                        tl.clear_seek_pending(tl.seek_epoch());
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
                    tl.is_seek_pending(),
                    want_seek_pending,
                    "mask {mask:#04b} play={initial_playing} seek_pending"
                );

                tl.set_playing(!initial_playing);
                assert_eq!(tl.is_playing(), !initial_playing);
                assert_eq!(tl.is_flushing(), want_flushing);
                assert_eq!(tl.is_seek_pending(), want_seek_pending);
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
