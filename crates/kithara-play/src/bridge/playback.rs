use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use portable_atomic::{AtomicF64, AtomicU32};

/// Monotonic identity of one cross-plane session-seek preparation attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
#[doc(hidden)]
pub struct SessionSeekAttempt(NonZeroU64);

impl SessionSeekAttempt {
    pub(crate) fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    #[cfg(test)]
    pub(crate) const fn new_for_test(value: u64) -> Self {
        match NonZeroU64::new(value) {
            Some(value) => Self(value),
            None => panic!("session seek test attempt must be non-zero"),
        }
    }
}

/// Coherent snapshot of the live playback scalars.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct PlaybackSnapshot {
    /// Whether more than one player track is audible.
    #[field(get(copy, name = has_multiple_tracks))]
    pub(crate) multiple_tracks: bool,
    /// Whether playback is active.
    #[field(get(copy, name = is_playing))]
    pub(crate) playing: bool,
    /// Total media duration in seconds; `0.0` when unknown.
    #[field(get(copy))]
    pub(crate) duration: f64,
    /// Decoded-ahead frontier in seconds. Always `>= position`.
    #[field(get(copy))]
    pub(crate) frontier: f64,
    /// Playback position in seconds.
    #[field(get(copy))]
    pub(crate) position: f64,
    /// Current output sample rate.
    #[field(get(copy))]
    pub(crate) sample_rate: u32,
}

/// Atomic playback state written by the RT processor and read by control code.
#[derive(Default)]
#[non_exhaustive]
pub struct PlaybackShared {
    /// Whether more than one player track is audible.
    pub multiple_tracks: AtomicBool,
    /// Whether playback is active.
    pub playing: AtomicBool,
    /// Total media duration in seconds; `0.0` when unknown.
    pub duration: AtomicF64,
    /// Decoded-ahead frontier in seconds.
    pub frontier: AtomicF64,
    /// Playback position in seconds.
    pub position: AtomicF64,
    /// Current output sample rate.
    pub sample_rate: AtomicU32,
    /// Number of audio-thread process calls.
    pub process_count: AtomicU64,
    /// Current seek epoch used to invalidate stale seek requests.
    pub seek_epoch: AtomicU64,
    session_seek_attempt: AtomicU64,
    session_seek_cancelled: AtomicU64,
    session_seek_failed: AtomicU64,
    session_seek_prepared: AtomicU64,
}

impl PlaybackShared {
    pub(crate) fn acknowledge_session_seek_cancel(&self, attempt: SessionSeekAttempt) {
        let _ = self.session_seek_cancelled.compare_exchange(
            attempt.get(),
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub(crate) fn cancel_session_seek(&self, attempt: SessionSeekAttempt) {
        self.session_seek_cancelled
            .store(attempt.get(), Ordering::SeqCst);
    }

    pub(crate) fn clear_prepared_session_seek(&self, attempt: SessionSeekAttempt) {
        let _ = self.session_seek_prepared.compare_exchange(
            attempt.get(),
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub(crate) fn fail_session_seek(&self, attempt: SessionSeekAttempt) {
        self.session_seek_failed
            .store(attempt.get(), Ordering::SeqCst);
    }

    pub fn next_seek_epoch(&self) -> u64 {
        self.seek_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    pub(crate) fn next_session_seek_attempt(&self) -> Option<SessionSeekAttempt> {
        let previous = self
            .session_seek_attempt
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .ok()?;
        SessionSeekAttempt::new(previous.checked_add(1)?)
    }

    pub(crate) fn prepare_session_seek(&self, attempt: SessionSeekAttempt) {
        self.session_seek_prepared
            .store(attempt.get(), Ordering::SeqCst);
    }

    pub(crate) fn session_seek_cancelled(&self, attempt: SessionSeekAttempt) -> bool {
        self.session_seek_cancelled.load(Ordering::SeqCst) == attempt.get()
    }

    pub(crate) fn session_seek_failed(&self, attempt: SessionSeekAttempt) -> bool {
        self.session_seek_failed.load(Ordering::SeqCst) == attempt.get()
    }

    pub(crate) fn session_seek_prepared(&self, attempt: SessionSeekAttempt) -> bool {
        self.session_seek_prepared.load(Ordering::SeqCst) == attempt.get()
    }

    /// Single coherent read of the live playback scalars.
    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        let position = self.position.load(Ordering::Relaxed);
        let frontier = self.frontier.load(Ordering::Relaxed).max(position);
        PlaybackSnapshot {
            position,
            frontier,
            duration: self.duration.load(Ordering::Relaxed),
            sample_rate: self.sample_rate.load(Ordering::Relaxed),
            multiple_tracks: self.multiple_tracks.load(Ordering::SeqCst),
            playing: self.playing.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn playback_shared_defaults() {
        let playback = PlaybackShared::default();
        assert!(!playback.playing.load(Ordering::Relaxed));
        assert_eq!(playback.seek_epoch.load(Ordering::Relaxed), 0);
        assert_eq!(playback.position.load(Ordering::Relaxed), 0.0);
        assert_eq!(playback.duration.load(Ordering::Relaxed), 0.0);
        assert_eq!(playback.sample_rate.load(Ordering::Relaxed), 0);
        assert!(!playback.multiple_tracks.load(Ordering::Relaxed));
        let attempt = SessionSeekAttempt::new_for_test(1);
        assert!(!playback.session_seek_failed(attempt));
        assert!(!playback.session_seek_prepared(attempt));
        assert!(!playback.session_seek_cancelled(attempt));
    }

    #[kithara::test]
    fn playback_shared_seek_epoch_increments() {
        let playback = PlaybackShared::default();
        assert_eq!(playback.next_seek_epoch(), 1);
        assert_eq!(playback.next_seek_epoch(), 2);
        assert_eq!(playback.next_seek_epoch(), 3);
    }

    #[kithara::test]
    fn session_seek_status_is_scoped_to_attempt_identity() {
        let playback = PlaybackShared::default();
        let old = playback
            .next_session_seek_attempt()
            .expect("first attempt allocates");
        playback.prepare_session_seek(old);
        playback.fail_session_seek(old);

        let current = playback
            .next_session_seek_attempt()
            .expect("second attempt allocates");

        assert!(!playback.session_seek_prepared(current));
        assert!(!playback.session_seek_failed(current));
    }

    #[kithara::test]
    fn snapshot_reads_all_fields_at_once() {
        let playback = PlaybackShared::default();
        playback.playing.store(true, Ordering::Relaxed);
        playback.position.store(12.0, Ordering::Relaxed);
        playback.frontier.store(20.0, Ordering::Relaxed);
        playback.duration.store(180.0, Ordering::Relaxed);
        playback.sample_rate.store(48_000, Ordering::Relaxed);
        playback.multiple_tracks.store(true, Ordering::Relaxed);

        let snap = playback.snapshot();
        assert!(snap.playing);
        assert!((snap.position - 12.0).abs() < f64::EPSILON);
        assert!((snap.frontier - 20.0).abs() < f64::EPSILON);
        assert!((snap.duration - 180.0).abs() < f64::EPSILON);
        assert_eq!(snap.sample_rate, 48_000);
        assert!(snap.multiple_tracks);
    }

    #[kithara::test]
    fn snapshot_frontier_never_trails_position() {
        let playback = PlaybackShared::default();
        playback.position.store(0.917, Ordering::Relaxed);
        playback.frontier.store(0.657, Ordering::Relaxed);

        let snap = playback.snapshot();
        assert!(
            snap.frontier >= snap.position,
            "frontier {} must cover position {}",
            snap.frontier,
            snap.position
        );
    }
}
