#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize},
};

use kithara_platform::{Condvar, tokio, tokio::sync::Notify};
use kithara_stream::{DemandSlot, Timeline, TransferCoordination};
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
    pub reader_advanced: Notify,
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
            reader_advanced: Notify::new(),
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

    pub(crate) fn has_pending_segment_request(&self, seek_epoch: u64) -> bool {
        self.demand
            .peek()
            .is_some_and(|request| request.seek_epoch == seek_epoch)
    }

    pub(crate) fn clear_pending_segment_request(&self, request: SegmentRequest) {
        if self.demand.peek() == Some(request) {
            self.demand.clear();
        }
    }

    pub(crate) fn take_segment_request(&self) -> Option<SegmentRequest> {
        self.demand.take()
    }

    pub(crate) fn requeue_segment_request(&self, request: SegmentRequest) {
        self.demand.replace(request);
        self.reader_advanced.notify_one();
    }

    pub(crate) fn clear_segment_requests(&self) {
        self.demand.clear();
    }

    #[must_use]
    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    pub(crate) fn notified_reader_advanced(&self) -> tokio::sync::futures::Notified<'_> {
        self.reader_advanced.notified()
    }
}

impl TransferCoordination<SegmentRequest> for HlsCoord {
    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn demand(&self) -> &DemandSlot<SegmentRequest> {
        &self.demand
    }
}
