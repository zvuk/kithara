#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize},
};

use kithara_platform::{Condvar, tokio::sync::Notify};
use kithara_stream::{DemandSlot, Timeline};
use tokio_util::sync::CancellationToken;

use crate::ids::{SegmentIndex, VariantIndex};

/// Request to load a specific segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRequest {
    pub segment_index: SegmentIndex,
    pub variant: VariantIndex,
    pub seek_epoch: u64,
}

pub struct HlsCoord {
    pub abr_variant_index: Arc<AtomicUsize>,
    pub cancel: CancellationToken,
    pub condvar: Condvar,
    pub had_midstream_switch: AtomicBool,
    /// `Arc<Notify>` so the handle can be cloned out to components that
    /// need an owned `'static` wake primitive (e.g. a future
    /// `Stream<Item = FetchCmd>` adapter that registers the notify in
    /// its own `poll_next` context). All existing `.notify_one()` and
    /// `.notified()` calls continue to work via `Deref` to [`Notify`].
    pub reader_advanced: Arc<Notify>,
    pub stopped: AtomicBool,
    timeline: Timeline,
    demand: DemandSlot<SegmentRequest>,
}

impl HlsCoord {
    #[must_use]
    pub fn new(
        cancel: CancellationToken,
        timeline: Timeline,
        abr_variant_index: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            abr_variant_index,
            cancel,
            condvar: Condvar::new(),
            had_midstream_switch: AtomicBool::new(false),
            reader_advanced: Arc::new(Notify::new()),
            stopped: AtomicBool::new(false),
            timeline,
            demand: DemandSlot::new(),
        }
    }

    pub(crate) fn enqueue_segment_request(&self, request: SegmentRequest) -> bool {
        let inserted = self.demand.submit(request);
        self.reader_advanced.notify_one();
        inserted
    }

    pub(crate) fn clear_pending_segment_request(&self, request: SegmentRequest) {
        if self.demand.peek() == Some(request) {
            self.demand.clear();
        }
    }

    pub fn take_segment_request(&self) -> Option<SegmentRequest> {
        self.demand.take()
    }

    pub fn peek_segment_request(&self) -> Option<SegmentRequest> {
        self.demand.peek()
    }

    pub(crate) fn clear_segment_requests(&self) {
        self.demand.clear();
    }

    #[must_use]
    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }
}
