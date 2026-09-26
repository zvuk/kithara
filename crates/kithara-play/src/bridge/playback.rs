use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use portable_atomic::{AtomicF32, AtomicF64, AtomicU32};

use super::RtMetrics;

/// One read of each live playback scalar.
///
/// The fields are independent relaxed loads and can straddle two audio blocks, so a consumer
/// needing two of them to agree must derive both from one field.
#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct PlaybackSnapshot {
    /// Whether playback is active.
    #[field(get = is_playing)]
    pub(crate) playing: bool,
    /// Effective media seconds consumed per output second; `0.0` while paused.
    pub(crate) rate: f32,
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
}

impl PlaybackSnapshot {
    /// The same read with nothing audible in it.
    ///
    /// The RT processor publishes `playing` and `rate` as it runs. An output
    /// the platform has suspended never schedules it, so both fields keep the
    /// value they held when the audio stopped; this is what they mean once
    /// nothing reaches the speakers.
    #[must_use]
    pub(crate) const fn silenced(self) -> Self {
        Self {
            playing: false,
            rate: 0.0,
            ..self
        }
    }
}

/// Atomic playback state written by the RT processor and read by control code.
#[derive(Default)]
#[non_exhaustive]
pub struct PlaybackShared {
    /// Whether playback is active.
    pub playing: AtomicBool,
    /// Cached span in seconds: how much of the source is on disk.
    pub(crate) cached: AtomicF64,
    /// Total media duration in seconds; `0.0` when unknown.
    pub(crate) duration: AtomicF64,
    /// Decoded-ahead frontier in seconds.
    pub(crate) frontier: AtomicF64,
    /// Playback position in seconds.
    pub(crate) position: AtomicF64,
    /// Current output sample rate.
    pub sample_rate: AtomicU32,
    /// Number of audio-thread process calls.
    pub process_count: AtomicU64,
    /// Current seek epoch used to invalidate stale seek requests.
    pub seek_epoch: AtomicU64,
    /// Effective media seconds consumed per output second; `0.0` while paused.
    pub(crate) rate: AtomicF32,
    /// Epoch of the last item the control side made leading.
    leading_epoch: AtomicU64,
    /// Duration the control side declared for that item.
    leading_duration: AtomicF64,
    /// Epoch of the leading item the audio thread has taken on; `position` and `duration`
    /// describe that item.
    adopted_epoch: AtomicU64,
    metrics: RtMetrics,
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

    /// Make a new item leading, ahead of the `FadeIn` that carries the returned epoch to the
    /// audio thread.
    ///
    /// Until the audio thread adopts that epoch, a snapshot describes the new item at its head
    /// with `duration`: the blocks rendered meanwhile still publish the item they were leading.
    pub(crate) fn lead(&self, duration: f64) -> u64 {
        self.leading_duration
            .store(duration.max(0.0), Ordering::Relaxed);
        self.leading_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    /// Audio thread: take on the item `epoch` made leading, publishing its playhead with it.
    pub(crate) fn adopt(&self, epoch: u64, position: f64, duration: f64) {
        self.position.store(position, Ordering::Relaxed);
        self.duration.store(duration, Ordering::Relaxed);
        self.adopted_epoch.store(epoch, Ordering::Release);
    }

    /// Read every live playback scalar once. See [`PlaybackSnapshot`] for what the fields do and do
    /// not guarantee about each other.
    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        let leading = self.leading_epoch.load(Ordering::Acquire);
        let rate = self.rate.load(Ordering::Relaxed);
        let sample_rate = self.sample_rate.load(Ordering::Relaxed);
        let playing = self.playing.load(Ordering::Relaxed);
        if self.adopted_epoch.load(Ordering::Acquire) != leading {
            return PlaybackSnapshot {
                playing,
                rate,
                sample_rate,
                duration: self.leading_duration.load(Ordering::Relaxed),
                ..PlaybackSnapshot::default()
            };
        }
        let position = self.position.load(Ordering::Relaxed);
        let frontier = self.frontier.load(Ordering::Relaxed).max(position);
        PlaybackSnapshot {
            position,
            frontier,
            playing,
            rate,
            sample_rate,
            cached: self.cached.load(Ordering::Relaxed),
            duration: self.duration.load(Ordering::Relaxed),
        }
    }

    /// Withdraw an epoch whose `PlayerCmd::Seek` never reached the processor.
    ///
    /// Publishing promises the processor a re-base, and a track holds its
    /// natural end while a published seek outranks it. A send that fails leaves
    /// nothing to carry the promise, so it has to be taken back or the track
    /// would hold that end forever.
    ///
    /// Withdrawal succeeds only while this epoch is still the published one: a
    /// newer seek has its own command in flight, and rolling back over it would
    /// strand that one instead.
    pub fn withdraw_seek_epoch(&self, epoch: u64) {
        self.seek_epoch
            .compare_exchange(
                epoch,
                epoch.wrapping_sub(1),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .ok();
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
        assert_eq!(playback.rate.load(Ordering::Relaxed), 0.0);
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
    fn withdrawing_the_newest_epoch_unpublishes_the_seek() {
        let playback = PlaybackShared::default();
        let epoch = playback.next_seek_epoch();
        playback.withdraw_seek_epoch(epoch);
        assert_eq!(playback.seek_epoch.load(Ordering::SeqCst), 0);
    }

    #[kithara::test]
    fn withdrawing_an_overtaken_epoch_leaves_the_newer_seek_published() {
        let playback = PlaybackShared::default();
        let overtaken = playback.next_seek_epoch();
        let newest = playback.next_seek_epoch();
        playback.withdraw_seek_epoch(overtaken);
        assert_eq!(playback.seek_epoch.load(Ordering::SeqCst), newest);
    }

    #[kithara::test]
    fn snapshot_reads_all_fields_at_once() {
        let playback = PlaybackShared::default();
        playback.playing.store(true, Ordering::Relaxed);
        playback.position.store(12.0, Ordering::Relaxed);
        playback.frontier.store(20.0, Ordering::Relaxed);
        playback.duration.store(180.0, Ordering::Relaxed);
        playback.rate.store(1.25, Ordering::Relaxed);
        playback.sample_rate.store(48_000, Ordering::Relaxed);

        let snap = playback.snapshot();
        assert!(snap.playing);
        assert!((snap.position - 12.0).abs() < f64::EPSILON);
        assert!((snap.frontier - 20.0).abs() < f64::EPSILON);
        assert!((snap.duration - 180.0).abs() < f64::EPSILON);
        assert!((snap.rate - 1.25).abs() < f32::EPSILON);
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
