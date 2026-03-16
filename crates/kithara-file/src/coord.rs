#![forbid(unsafe_code)]

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_platform::{tokio, tokio::sync::Notify};
use kithara_stream::{DemandSlot, Timeline, TransferCoordination};

pub struct FileCoord {
    demand: DemandSlot<Range<u64>>,
    downloader_wake: Notify,
    reader_advanced: Notify,
    read_pos: Arc<AtomicU64>,
    timeline: Timeline,
    total_bytes: Arc<AtomicU64>,
}

impl FileCoord {
    const NO_TOTAL_BYTES: u64 = u64::MAX;

    #[must_use]
    pub(crate) fn new(timeline: Timeline) -> Self {
        Self {
            demand: DemandSlot::new(),
            downloader_wake: Notify::new(),
            reader_advanced: Notify::new(),
            read_pos: Arc::new(AtomicU64::new(0)),
            timeline,
            total_bytes: Arc::new(AtomicU64::new(Self::NO_TOTAL_BYTES)),
        }
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(crate) fn read_pos(&self) -> u64 {
        self.read_pos.load(Ordering::Acquire)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(crate) fn set_read_pos(&self, value: u64) {
        self.read_pos.store(value, Ordering::Release);
        self.reader_advanced.notify_one();
    }

    #[must_use]
    pub(crate) fn download_pos(&self) -> u64 {
        self.timeline.download_position()
    }

    pub(crate) fn set_download_pos(&self, value: u64) {
        self.timeline.set_download_position(value);
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

    pub(crate) fn set_total_bytes(&self, total: Option<u64>) {
        self.total_bytes
            .store(total.unwrap_or(Self::NO_TOTAL_BYTES), Ordering::Release);
    }

    #[must_use]
    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    pub(crate) fn request_range(&self, range: Range<u64>) -> bool {
        let inserted = self.demand.submit(range);
        if inserted {
            self.downloader_wake.notify_one();
        }
        inserted
    }

    pub(crate) fn take_range_request(&self) -> Option<Range<u64>> {
        self.demand.take()
    }

    pub(crate) fn notified_reader_advance(&self) -> tokio::sync::futures::Notified<'_> {
        self.reader_advanced.notified()
    }

    pub(crate) fn notified_downloader_wake(&self) -> tokio::sync::futures::Notified<'_> {
        self.downloader_wake.notified()
    }
}

impl Default for FileCoord {
    fn default() -> Self {
        Self::new(Timeline::new())
    }
}

impl TransferCoordination<Range<u64>> for FileCoord {
    fn timeline(&self) -> Timeline {
        self.timeline()
    }

    fn demand(&self) -> &DemandSlot<Range<u64>> {
        &self.demand
    }
}
