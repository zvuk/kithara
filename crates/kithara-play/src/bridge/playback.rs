use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering, fence},
};

use kithara_audio::{PresentationCursor, PresentationPoint, SessionFrame};
use kithara_platform::sync::Arc;
use portable_atomic::{AtomicF64, AtomicU32};

use super::RtMetrics;

/// One read of each live playback scalar.
///
/// The scalar fields are independent relaxed loads and can straddle two audio
/// blocks, so a consumer needing two of them to agree must derive both from one
/// field. [`Self::presentation`] is a separate coherent snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct PlaybackSnapshot {
    /// Whether playback is active.
    #[field(get = is_playing)]
    pub(crate) playing: bool,
    /// Cached span in seconds: how much of the source is on disk. Independent
    /// of `frontier` — bytes land ahead of the decoder, and the decoder can run
    /// ahead of what the download side has reported.
    pub(crate) cached: f64,
    /// Total media duration in seconds; `0.0` when unknown.
    pub(crate) duration: f64,
    /// Decoded-ahead frontier in seconds. Always `>= position`.
    pub(crate) frontier: f64,
    /// Playback position in seconds.
    pub(crate) position: f64,
    /// Current output sample rate.
    pub(crate) sample_rate: u32,
    /// Latest leading-track producer point mapped to the session clock.
    #[field(get, copy)]
    pub(crate) presentation: Option<PresentationCursor>,
}

/// Atomic playback state written by the RT processor and read by control code.
#[derive(Default)]
#[non_exhaustive]
pub struct PlaybackShared {
    /// Whether playback is active.
    pub playing: AtomicBool,
    /// Cached span in seconds: how much of the source is on disk.
    pub cached: AtomicF64,
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
    presentation: AtomicPresentationCursor,
    metrics: RtMetrics,
}

#[derive(Default)]
struct AtomicPresentationCursor {
    epoch: AtomicU64,
    source_frame: AtomicU64,
    generation: AtomicU64,
    output_end: AtomicU64,
    sample_rate: AtomicU32,
    session_frame: AtomicI64,
    valid: AtomicBool,
    revision: AtomicU64,
}

impl AtomicPresentationCursor {
    fn load(&self) -> Option<PresentationCursor> {
        let before = self.revision.load(Ordering::Acquire);
        if before & 1 != 0 {
            return None;
        }

        let valid = self.valid.load(Ordering::Relaxed);
        let sample_rate = NonZeroU32::new(self.sample_rate.load(Ordering::Relaxed))?;
        let point = PresentationPoint::new(
            self.epoch.load(Ordering::Relaxed),
            self.source_frame.load(Ordering::Relaxed),
            self.generation.load(Ordering::Relaxed),
            self.output_end.load(Ordering::Relaxed),
            sample_rate,
        );
        let session_frame = SessionFrame::new(self.session_frame.load(Ordering::Relaxed));
        fence(Ordering::Acquire);
        let after = self.revision.load(Ordering::Acquire);
        if before != after || !valid {
            return None;
        }
        Some(PresentationCursor::new(point, session_frame))
    }

    fn store(&self, cursor: Option<PresentationCursor>) {
        let start = self.revision.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(start & 1, 0, "playback presentation has one writer");
        if let Some(cursor) = cursor {
            let point = cursor.point();
            self.epoch.store(point.seek_epoch(), Ordering::Relaxed);
            self.source_frame
                .store(point.source_frame(), Ordering::Relaxed);
            self.generation.store(point.generation(), Ordering::Relaxed);
            self.output_end.store(point.output_end(), Ordering::Relaxed);
            self.sample_rate
                .store(point.sample_rate().get(), Ordering::Relaxed);
            self.session_frame
                .store(i64::from(cursor.session_frame()), Ordering::Relaxed);
            self.valid.store(true, Ordering::Relaxed);
        } else {
            self.valid.store(false, Ordering::Relaxed);
        }
        self.revision
            .store(start.wrapping_add(2), Ordering::Release);
    }
}

/// Sole publisher for one playback slot's presentation coordinate.
///
/// This handle is intentionally not cloneable, and publishing requires
/// mutable access. The playback slot transfers it to exactly one processor.
pub(crate) struct PlaybackPresentationPublisher {
    playback: Arc<PlaybackShared>,
}

impl PlaybackPresentationPublisher {
    pub(crate) fn new(playback: Arc<PlaybackShared>) -> Self {
        Self { playback }
    }

    pub(crate) fn publish(&mut self, cursor: Option<PresentationCursor>) {
        self.playback.presentation.store(cursor);
    }
}

impl PlaybackShared {
    /// Lock-free counters the audio thread bumps instead of emitting `tracing` events.
    #[must_use]
    pub const fn metrics(&self) -> &RtMetrics {
        &self.metrics
    }

    pub fn next_seek_epoch(&self) -> u64 {
        self.seek_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    /// Read every live playback scalar once. See [`PlaybackSnapshot`] for what the fields do and do
    /// not guarantee about each other.
    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        let position = self.position.load(Ordering::Relaxed);
        let frontier = self.frontier.load(Ordering::Relaxed).max(position);
        PlaybackSnapshot {
            position,
            frontier,
            cached: self.cached.load(Ordering::Relaxed),
            duration: self.duration.load(Ordering::Relaxed),
            sample_rate: self.sample_rate.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
            presentation: self.presentation.load(),
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
        assert_eq!(playback.snapshot().presentation(), None);
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

    #[kithara::test]
    fn presentation_snapshot_round_trips_and_invalidates_as_one_value() {
        let playback = Arc::new(PlaybackShared::default());
        let mut publisher = PlaybackPresentationPublisher::new(Arc::clone(&playback));
        let point = PresentationPoint::new(
            7,
            9_600,
            3,
            192,
            NonZeroU32::new(48_000).expect("fixture rate is non-zero"),
        );
        let cursor = PresentationCursor::new(point, SessionFrame::new(10_256));

        publisher.publish(Some(cursor));
        assert_eq!(playback.snapshot().presentation(), Some(cursor));

        publisher.publish(None);
        assert_eq!(playback.snapshot().presentation(), None);
    }

    #[kithara::test]
    fn a_busy_or_changed_presentation_snapshot_never_spins() {
        let playback = PlaybackShared::default();
        playback.presentation.revision.store(1, Ordering::Release);

        assert_eq!(playback.snapshot().presentation(), None);
    }
}
