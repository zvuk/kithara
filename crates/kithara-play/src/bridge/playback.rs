use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use portable_atomic::{AtomicF64, AtomicU32};

/// Coherent snapshot of the live playback scalars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PlaybackSnapshot {
    /// Whether playback is active.
    pub(crate) playing: bool,
    /// Total media duration in seconds; `0.0` when unknown.
    pub(crate) duration: f64,
    /// Decoded-ahead frontier in seconds. Always `>= position`.
    pub(crate) frontier: f64,
    /// Playback position in seconds.
    pub(crate) position: f64,
    /// Current output sample rate.
    pub(crate) sample_rate: u32,
}

/// Atomic playback state written by the RT processor and read by control code.
#[derive(Default)]
#[non_exhaustive]
pub struct PlaybackShared {
    /// Whether playback is active.
    pub playing: AtomicBool,
    /// Playback position in seconds.
    pub position: AtomicF64,
    /// Decoded-ahead frontier in seconds.
    pub frontier: AtomicF64,
    /// Total media duration in seconds; `0.0` when unknown.
    pub duration: AtomicF64,
    /// Current output sample rate.
    pub sample_rate: AtomicU32,
    /// Number of audio-thread process calls.
    pub process_count: AtomicU64,
    /// Current seek epoch used to invalidate stale seek requests.
    pub seek_epoch: AtomicU64,
}

impl PlaybackSnapshot {
    /// Whether playback is active.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Total media duration in seconds; `0.0` when unknown.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Decoded-ahead frontier in seconds. Always `>= position`.
    #[must_use]
    pub fn frontier(&self) -> f64 {
        self.frontier
    }

    /// Playback position in seconds.
    #[must_use]
    pub fn position(&self) -> f64 {
        self.position
    }

    /// Current output sample rate.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl PlaybackShared {
    pub fn next_seek_epoch(&self) -> u64 {
        self.seek_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
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
    }

    #[kithara::test]
    fn playback_shared_seek_epoch_increments() {
        let playback = PlaybackShared::default();
        assert_eq!(playback.next_seek_epoch(), 1);
        assert_eq!(playback.next_seek_epoch(), 2);
        assert_eq!(playback.next_seek_epoch(), 3);
    }

    #[kithara::test]
    fn snapshot_reads_all_fields_at_once() {
        let playback = PlaybackShared::default();
        playback.playing.store(true, Ordering::Relaxed);
        playback.position.store(12.0, Ordering::Relaxed);
        playback.frontier.store(20.0, Ordering::Relaxed);
        playback.duration.store(180.0, Ordering::Relaxed);
        playback.sample_rate.store(48_000, Ordering::Relaxed);

        let snap = playback.snapshot();
        assert!(snap.playing);
        assert!((snap.position - 12.0).abs() < f64::EPSILON);
        assert!((snap.frontier - 20.0).abs() < f64::EPSILON);
        assert!((snap.duration - 180.0).abs() < f64::EPSILON);
        assert_eq!(snap.sample_rate, 48_000);
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
