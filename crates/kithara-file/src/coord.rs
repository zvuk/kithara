#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_platform::tokio as platform_tokio;
use kithara_stream::Timeline;
use platform_tokio::sync::Notify;

use crate::demand::DemandSlot;

pub(crate) struct FileCoord {
    /// Authoritative byte cursor exposed via
    /// [`Source::position`](kithara_stream::Source::position) — File owns
    /// its own atomic, lock-free for both reader and downloader threads.
    position: Arc<AtomicU64>,
    read_pos: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
    demand: DemandSlot<Range<u64>>,
    downloader_wake: Notify,
    reader_advanced: Notify,
    timeline: Timeline,
}

impl FileCoord {
    /// Sentinel for "total length unknown" stored in `total_bytes`
    /// (atomic). `set_total_bytes` replaces it once the HEAD/Range
    /// response settles; `total_bytes()` filters it back to `None`.
    const NO_TOTAL_BYTES: u64 = u64::MAX;

    #[must_use]
    pub(crate) fn new(timeline: Timeline) -> Self {
        Self {
            timeline,
            demand: DemandSlot::new(),
            downloader_wake: Notify::new(),
            position: Arc::new(AtomicU64::new(0)),
            read_pos: Arc::new(AtomicU64::new(0)),
            reader_advanced: Notify::new(),
            total_bytes: Arc::new(AtomicU64::new(Self::NO_TOTAL_BYTES)),
        }
    }

    #[must_use]
    pub(crate) fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    pub(crate) fn advance_position(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    pub(crate) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[must_use]
    pub(crate) fn position_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.position)
    }

    /// Borrow the demand notify — callers await `.notified()` directly.
    pub(crate) fn demand_notify(&self) -> &Notify {
        &self.downloader_wake
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(crate) fn read_pos(&self) -> u64 {
        self.read_pos.load(Ordering::Acquire)
    }

    pub(crate) fn request_range(&self, range: Range<u64>) -> bool {
        let inserted = self.demand.did_replace(range);
        if inserted {
            self.downloader_wake.notify_one();
        }
        inserted
    }

    pub(crate) fn set_download_pos(&self, value: u64) {
        self.timeline.set_download_position(value);
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

    pub(crate) fn take_range_request(&self) -> Option<Range<u64>> {
        self.demand.take()
    }

    #[must_use]
    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
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
        Self::new(Timeline::new())
    }
}
