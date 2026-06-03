#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_platform::tokio as platform_tokio;
use kithara_stream::{
    Activity, PlayheadRead, PlayheadState, PlayheadWrite, SeekControl, SeekObserve, SeekState,
};
use kithara_test_utils::kithara;
use platform_tokio::sync::Notify;

pub(crate) struct FileCoord {
    /// Authoritative byte cursor exposed via
    /// [`Source::position`](kithara_stream::Source::position) — File owns
    /// its own atomic, lock-free for both reader and downloader threads.
    position: Arc<AtomicU64>,
    read_pos: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
    reader_advanced: Notify,
    /// Backing playhead state — the coord owns the `Arc` directly and
    /// vends narrow trait-object handles from it.
    playhead: Arc<PlayheadState>,
    /// Backing seek/activity state — the coord owns the `Arc` directly and
    /// vends narrow trait-object handles from it.
    seek: Arc<SeekState>,
    /// Narrow seek-observe handle (flush gate) — derived from the shared
    /// `SeekState`, so it observes the same flags without the wide type.
    seek_obs: Arc<dyn SeekObserve>,
    /// Narrow activity handle (`is_playing`) read by the downloader peer.
    activity: Arc<dyn Activity>,
}

impl FileCoord {
    /// Sentinel for "total length unknown" stored in `total_bytes`
    /// (atomic). `set_total_bytes` replaces it once the HEAD/Range
    /// response settles; `total_bytes()` filters it back to `None`.
    const NO_TOTAL_BYTES: u64 = u64::MAX;

    #[must_use]
    pub(crate) fn new(playhead: Arc<PlayheadState>, seek: Arc<SeekState>) -> Self {
        let seek_obs = Arc::clone(&seek) as Arc<dyn SeekObserve>;
        let activity = Arc::clone(&seek) as Arc<dyn Activity>;
        Self {
            playhead,
            seek,
            seek_obs,
            activity,
            position: Arc::new(AtomicU64::new(0)),
            read_pos: Arc::new(AtomicU64::new(0)),
            reader_advanced: Notify::new(),
            total_bytes: Arc::new(AtomicU64::new(Self::NO_TOTAL_BYTES)),
        }
    }

    pub(crate) fn advance_position(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    #[must_use]
    pub(crate) fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(crate) fn read_pos(&self) -> u64 {
        self.read_pos.load(Ordering::Acquire)
    }

    /// Shared reader-position cell handed to the demand index so the
    /// elected producer can read the consumer's advances directly.
    #[must_use]
    pub(crate) fn read_pos_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.read_pos)
    }

    /// Report the current download byte position. The value is not stored
    /// on the coord — it exists only as a USDT probe point
    /// (`#[kithara::probe]`) for download-progress observability.
    #[kithara::probe(value)]
    pub(crate) fn set_download_pos(&self, value: u64) {
        let _ = value;
    }

    pub(crate) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(crate) fn set_read_pos(&self, value: u64) {
        self.read_pos.store(value, Ordering::Release);
        self.reader_advanced.notify_one();
    }

    pub(crate) fn set_total_bytes(&self, total: Option<u64>) {
        self.total_bytes
            .store(total.unwrap_or(Self::NO_TOTAL_BYTES), Ordering::Release);
    }

    #[must_use]
    pub(crate) fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    #[must_use]
    pub(crate) fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    #[must_use]
    pub(crate) fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    #[must_use]
    pub(crate) fn seek_control(&self) -> Arc<dyn SeekControl> {
        Arc::clone(&self.seek) as Arc<dyn SeekControl>
    }

    #[must_use]
    pub(crate) fn activity_handle(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    #[must_use]
    pub(crate) fn seek_epoch_handle(&self) -> Arc<AtomicU64> {
        self.seek.seek_epoch_arc()
    }

    #[must_use]
    pub(crate) fn seek_obs(&self) -> &Arc<dyn SeekObserve> {
        &self.seek_obs
    }

    #[must_use]
    pub(crate) fn activity(&self) -> &Arc<dyn Activity> {
        &self.activity
    }

    #[must_use]
    pub(crate) fn total_bytes(&self) -> Option<u64> {
        let total = self.total_bytes.load(Ordering::Acquire);
        if total == Self::NO_TOTAL_BYTES {
            None
        } else {
            Some(total)
        }
    }
}

impl Default for FileCoord {
    fn default() -> Self {
        Self::new(Arc::new(PlayheadState::new()), Arc::new(SeekState::new()))
    }
}
