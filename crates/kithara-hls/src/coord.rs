#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_abr::AbrState;
use kithara_platform::{Condvar, tokio::sync::Notify};
use kithara_stream::Timeline;
use tokio_util::sync::CancellationToken;

use crate::{
    demand::DemandSlot,
    ids::{SegmentIndex, VariantIndex},
};

/// Request to load a specific segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRequest {
    pub segment_index: SegmentIndex,
    pub variant: VariantIndex,
    pub seek_epoch: u64,
}

#[cfg(any(test, feature = "test-utils"))]
impl kithara_test_utils::probes::IntoProbeArg for SegmentRequest {
    /// Pack `(variant, segment_index)` into a single `u64` so the field
    /// can ride a `#[kithara::probe(request, …)]` site without
    /// unpacking the struct in the production signature. `seek_epoch`
    /// is dropped — it is not load-bearing for the seg-routing
    /// diagnostics that consume this probe (`commit_fetch_inline`
    /// epoch handling fires its own `seek_epoch_reset` probe). Layout:
    ///
    /// ```text
    /// bits 63..32  variant
    /// bits 31..00  segment_index
    /// ```
    ///
    /// `from_probe_arg` round-trips `(variant, segment_index)` and
    /// reconstructs `SegmentRequest` with `seek_epoch = 0` (the lossy
    /// field documented above) so tests can read it via
    /// `SegmentRequest::from_probe_arg(event.u64("request").unwrap())`
    /// without writing a private decode helper.
    fn into_probe_arg(self) -> u64 {
        ((self.variant as u64) << 32) | (self.segment_index as u64 & 0xFFFF_FFFF)
    }

    fn from_probe_arg(packed: u64) -> Self {
        Self {
            variant: (packed >> 32) as VariantIndex,
            segment_index: (packed & 0xFFFF_FFFF) as SegmentIndex,
            seek_epoch: 0,
        }
    }
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
    /// Authoritative byte cursor for this HLS source (transitional
    /// home — Plan 03 moves ownership to the active `HlsVariant`).
    position: Arc<AtomicU64>,
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
            position: Arc::new(AtomicU64::new(0)),
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

    pub(crate) fn clear_pending_segment_request(&self, request: SegmentRequest) {
        if self.demand.peek() == Some(request) {
            self.demand.clear();
        }
    }

    pub(crate) fn clear_segment_requests(&self) {
        self.demand.clear();
    }

    pub(crate) fn enqueue_segment_request(&self, request: SegmentRequest) -> bool {
        let inserted = self.demand.did_replace(request);
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
