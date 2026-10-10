#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use kithara_platform::sync::{Arc, Mutex, Notify};
use kithara_stream::{Activity, ActivityWriter, PlayheadRead, PlayheadState, PlayheadWrite};
use kithara_test_utils::kithara;

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct FileCoord {
    /// Narrow activity handle (`is_playing`) read by the downloader peer.
    #[field(get, vis = "pub(crate)", deref = false)]
    activity: Activity,
    /// Backing playhead state — the coord owns the `Arc` directly and
    /// vends narrow trait-object handles from it.
    playhead: Arc<PlayheadState>,
    /// Authoritative byte cursor exposed via
    /// [`Source::position`](kithara_stream::Source::position) — File owns
    /// its own atomic, lock-free for both reader and downloader threads.
    position: Arc<AtomicU64>,
    read_pos: Arc<AtomicU64>,
    activity_writer: Mutex<Option<ActivityWriter>>,
    total_bytes: Arc<AtomicU64>,
    reader_advanced: Notify,
}

impl FileCoord {
    /// Sentinel for "total length unknown" stored in `total_bytes`
    /// (atomic). `set_total_bytes` replaces it once the HEAD/Range
    /// response settles; `total_bytes()` filters it back to `None`.
    const NO_TOTAL_BYTES: u64 = u64::MAX;

    #[must_use]
    pub(crate) fn new(playhead: Arc<PlayheadState>) -> Self {
        let writer = ActivityWriter::new();
        let activity = writer.reader();
        Self {
            playhead,
            activity_writer: Mutex::new(Some(writer)),
            activity,
            position: Arc::new(AtomicU64::new(0)),
            read_pos: Arc::new(AtomicU64::new(0)),
            reader_advanced: Notify::default(),
            total_bytes: Arc::new(AtomicU64::new(Self::NO_TOTAL_BYTES)),
        }
    }

    #[must_use]
    pub(crate) fn activity_handle(&self) -> Activity {
        self.activity.clone()
    }

    pub(crate) fn take_activity_writer(&self) -> Option<ActivityWriter> {
        self.activity_writer.lock().take()
    }

    pub(crate) fn advance_position(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
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
    pub(crate) fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    #[kithara::measure]
    pub(crate) fn read_pos(&self) -> u64 {
        self.read_pos.load(Ordering::Acquire)
    }

    /// Shared reader-position cell handed to the demand index so the
    /// elected producer can read the consumer's advances directly.
    #[must_use]
    pub(crate) fn read_pos_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.read_pos)
    }

    /// Report the current download byte position: the contiguous prefix the
    /// peer has landed in the asset store. Doubles as a USDT probe point
    /// (`#[kithara::probe]`) for download-progress observability.
    ///
    /// This is the write cursor of the running fetch, which after a seek
    /// re-anchors on the new reader position — the playhead turns it into the
    /// timeline span a host progress bar reads.
    #[kithara::probe(value)]
    pub(crate) fn set_download_pos(&self, value: u64) {
        if let Some(total) = self.total_bytes() {
            self.playhead.set_cached(value, total);
        }
    }

    pub(crate) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[kithara::measure]
    pub(crate) fn set_read_pos(&self, value: u64) {
        self.read_pos.store(value, Ordering::Release);
        self.reader_advanced.notify_one();
    }

    pub(crate) fn set_total_bytes(&self, total: Option<u64>) {
        self.total_bytes
            .store(total.unwrap_or(Self::NO_TOTAL_BYTES), Ordering::Release);
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
        Self::new(Arc::new(PlayheadState::new()))
    }
}
