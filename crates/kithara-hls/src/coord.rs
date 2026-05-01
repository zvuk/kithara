#![forbid(unsafe_code)]

use std::sync::{Arc, atomic::AtomicBool};

use kithara_abr::AbrState;
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
    /// Shared ABR state — the only authoritative source of the current
    /// variant index. Held as an `Arc<AbrState>` so HLS reads stay
    /// lock-free (`current_variant_index()` is one `Acquire` atomic load)
    /// while writes are gated behind `pub(crate)` `apply()` inside
    /// `kithara-abr` — HLS cannot bypass the controller.
    pub abr_state: Arc<AbrState>,
    /// `Arc<Notify>` so the handle can be cloned out to components that
    /// need an owned `'static` wake primitive (e.g. a future
    /// `Stream<Item = FetchCmd>` adapter that registers the notify in
    /// its own `poll_next` context). All existing `.notify_one()` and
    /// `.notified()` calls continue to work via `Deref` to [`Notify`].
    pub reader_advanced: Arc<Notify>,
    pub had_midstream_switch: AtomicBool,
    pub stopped: AtomicBool,
    pub cancel: CancellationToken,
    pub condvar: Condvar,
    demand: DemandSlot<SegmentRequest>,
    timeline: Timeline,
}

impl HlsCoord {
    #[must_use]
    pub fn new(cancel: CancellationToken, timeline: Timeline, abr_state: Arc<AbrState>) -> Self {
        Self {
            abr_state,
            cancel,
            timeline,
            condvar: Condvar::new(),
            had_midstream_switch: AtomicBool::new(false),
            reader_advanced: Arc::new(Notify::new()),
            stopped: AtomicBool::new(false),
            demand: DemandSlot::new(),
        }
    }

    pub(crate) fn clear_pending_segment_request(&self, request: SegmentRequest) {
        if self.demand.peek() == Some(request) {
            self.demand.clear();
        }
    }

    pub(crate) fn clear_segment_requests(&self) {
        self.demand.clear();
    }

    pub(crate) fn enqueue_segment_request(&self, request: SegmentRequest) -> bool {
        let inserted = self.demand.submit(request);
        self.reader_advanced.notify_one();
        inserted
    }

    pub fn peek_segment_request(&self) -> Option<SegmentRequest> {
        self.demand.peek()
    }

    pub fn take_segment_request(&self) -> Option<SegmentRequest> {
        self.demand.take()
    }

    #[must_use]
    pub(crate) fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    /// Read the current variant index from the shared ABR state.
    ///
    /// One `Acquire` atomic load — same hot-path cost as the previous
    /// `coord.abr_variant_index.load(Ordering::Acquire)` direct read.
    #[must_use]
    pub fn variant_index(&self) -> usize {
        self.abr_state.current_variant_index()
    }
}
