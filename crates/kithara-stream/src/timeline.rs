#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

/// Shared playback timeline used across stream layers.
///
/// Stores canonical byte position and committed playback position.
#[derive(Clone, Debug)]
pub struct Timeline {
    byte_position: Arc<AtomicU64>,
    committed_position_ns: Arc<AtomicU64>,
    download_position: Arc<AtomicU64>,
    eof: Arc<AtomicBool>,
    pending_seek_epoch: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
    total_duration_ns: Arc<AtomicU64>,

    // Seek coordinator fields (GStreamer FLUSH_START/STOP pattern)
    seek_epoch: Arc<AtomicU64>,
    flushing: Arc<AtomicBool>,
    seek_target_ns: Arc<AtomicU64>,
}

impl Timeline {
    const NO_PENDING_SEEK: u64 = u64::MAX;
    const NO_TOTAL_BYTES: u64 = u64::MAX;
    const NO_DURATION: u64 = u64::MAX;
    const NO_SEEK_TARGET: u64 = u64::MAX;

    #[must_use]
    pub fn new() -> Self {
        Self {
            byte_position: Arc::new(AtomicU64::new(0)),
            committed_position_ns: Arc::new(AtomicU64::new(0)),
            download_position: Arc::new(AtomicU64::new(0)),
            eof: Arc::new(AtomicBool::new(false)),
            pending_seek_epoch: Arc::new(AtomicU64::new(Self::NO_PENDING_SEEK)),
            total_bytes: Arc::new(AtomicU64::new(Self::NO_TOTAL_BYTES)),
            total_duration_ns: Arc::new(AtomicU64::new(Self::NO_DURATION)),
            seek_epoch: Arc::new(AtomicU64::new(0)),
            flushing: Arc::new(AtomicBool::new(false)),
            seek_target_ns: Arc::new(AtomicU64::new(Self::NO_SEEK_TARGET)),
        }
    }

    #[must_use]
    pub fn byte_position(&self) -> u64 {
        self.byte_position.load(Ordering::Acquire)
    }

    pub fn set_byte_position(&self, position: u64) {
        self.byte_position.store(position, Ordering::Release);
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
        self.eof.store(eof, Ordering::Release);
    }

    #[must_use]
    pub fn eof(&self) -> bool {
        self.eof.load(Ordering::Acquire)
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

    pub fn set_total_bytes(&self, total: Option<u64>) {
        self.total_bytes
            .store(total.unwrap_or(Self::NO_TOTAL_BYTES), Ordering::Release);
    }

    #[must_use]
    pub fn total_bytes(&self) -> Option<u64> {
        let total = self.total_bytes.load(Ordering::Acquire);
        if total == Self::NO_TOTAL_BYTES {
            None
        } else {
            Some(total)
        }
    }

    pub fn advance_committed_samples(
        &self,
        interleaved_samples: u64,
        sample_rate: u32,
        channels: u16,
    ) {
        if sample_rate == 0 || channels == 0 || interleaved_samples == 0 {
            return;
        }

        let frames = interleaved_samples / u64::from(channels);
        if frames == 0 {
            return;
        }

        let delta_nanos = (u128::from(frames) * 1_000_000_000u128) / u128::from(sample_rate);
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
        // flushing must be set LAST so readers see target before flushing flag
        self.flushing.store(true, Ordering::Release);
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
        self.flushing.store(false, Ordering::SeqCst);
        // Double-check: if a newer seek arrived while we were clearing,
        // its initiate_seek may have already set flushing=true which we
        // just overwrote. Re-set flushing to avoid losing the seek.
        if self.seek_epoch.load(Ordering::SeqCst) != epoch {
            self.flushing.store(true, Ordering::SeqCst);
        }
    }

    /// Check if the pipeline is being flushed (seek pending).
    #[must_use]
    pub fn is_flushing(&self) -> bool {
        self.flushing.load(Ordering::Acquire)
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
}
