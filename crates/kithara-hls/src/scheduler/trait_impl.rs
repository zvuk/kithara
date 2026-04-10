use std::{
    future::Future,
    sync::{Arc, atomic::Ordering},
};

use kithara_events::HlsEvent;
use kithara_platform::tokio::sync::Notify;
use tracing::{debug, trace};

use super::{
    helpers::is_stale_epoch,
    plan::{DemandOutcome, PlanOutcome},
    state::{HlsScheduler, VERBOSE_SEGMENT_LIMIT},
};
use crate::{
    coord::SegmentRequest,
    ids::{SegmentId, VariantIndex},
    loading::SegmentMeta,
};

impl Drop for HlsScheduler {
    fn drop(&mut self) {
        self.coord.stopped.store(true, Ordering::Release);
        self.coord.condvar.notify_all();
    }
}

/// Result of downloading a single HLS segment.
pub(crate) struct HlsFetch {
    pub(crate) init_len: u64,
    pub(crate) init_url: Option<url::Url>,
    pub(crate) media: SegmentMeta,
    pub(crate) media_cached: bool,
    pub(crate) segment: SegmentId,
    pub(crate) variant: VariantIndex,
    pub(crate) duration: std::time::Duration,
    pub(crate) seek_epoch: kithara_events::SeekEpoch,
}

impl HlsScheduler {
    /// Check for on-demand requests (e.g. seek) without blocking.
    pub(crate) fn poll_demand_next(&mut self) -> Option<DemandOutcome> {
        self.poll_demand_impl()
    }

    /// Plan the next work batch.
    pub(crate) fn plan_next(&mut self) -> PlanOutcome {
        self.plan_impl()
    }

    /// Wait until the throttle condition clears (reader advances enough).
    pub(crate) async fn wait_ready_future(&self) {
        if self.coord.timeline().is_flushing() || !self.should_throttle() {
            return;
        }
        self.coord.notified_reader_advanced().await;
    }

    /// Return a future that resolves when the reader signals forward
    /// progress — used by the worker to wait for demand wakes in idle
    /// state. Cloned [`Arc<HlsCoord>`] so the future is `'static` and
    /// can be composed in `tokio::select!` alongside other borrows.
    pub(crate) fn demand_signal_future(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let coord = Arc::clone(&self.coord);
        async move { coord.notified_reader_advanced().await }
    }

    /// Clone the demand-notify [`Arc<Notify>`] from the coord.
    ///
    /// Used by components that need an owned `'static` wake primitive
    /// independent of `&self` — e.g. a future `Stream<Item = FetchCmd>`
    /// adapter that stores the notify alongside its own state and calls
    /// `notified()` from inside its `poll_next` context. The existing
    /// worker loop keeps using [`wait_ready_future`](Self::wait_ready_future)
    /// / [`demand_signal_future`](Self::demand_signal_future) unchanged.
    #[must_use]
    #[expect(dead_code, reason = "Phase 4c.0.3 prep — Stream adapter will use this")]
    pub(crate) fn demand_notify(&self) -> Arc<Notify> {
        self.coord.demand_notify()
    }

    /// Commit a completed fetch (stale-check, classify, and apply to the
    /// shared [`StreamIndex`]). Replaces the deleted `Downloader::commit`.
    pub(crate) fn commit_fetch(&mut self, fetch: HlsFetch) {
        let current_epoch = self.coord.timeline().seek_epoch();
        let seg_idx = fetch.segment.media_index().unwrap_or(0);

        if is_stale_epoch(fetch.seek_epoch, current_epoch) {
            trace!(
                fetch_epoch = fetch.seek_epoch,
                current_epoch,
                variant = fetch.variant,
                segment_index = seg_idx,
                "dropping stale fetch before commit"
            );
            self.bus.publish(HlsEvent::StaleFetchDropped {
                seek_epoch: fetch.seek_epoch,
                current_epoch,
                variant: fetch.variant,
                segment_index: seg_idx,
            });
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_stale_cross_codec_fetch(&fetch) {
            debug!(
                variant = fetch.variant,
                segment_index = seg_idx,
                current_variant = self.abr.get_current_variant_index(),
                "dropping stale cross-codec fetch after switched anchor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_below_switch_floor(fetch.variant, seg_idx) {
            debug!(
                variant = fetch.variant,
                segment_index = seg_idx,
                floor = self.gap_scan_start_segment(),
                "dropping fetch below switched-layout floor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        let (is_variant_switch, is_midstream_switch) =
            self.classify_variant_transition(fetch.variant, seg_idx);

        if fetch.init_len > 0 {
            self.sent_init_for_variant.insert(fetch.variant);
        }

        if seg_idx == self.current_segment_index() {
            self.advance_current_segment_index(seg_idx + 1);
        }

        if seg_idx <= VERBOSE_SEGMENT_LIMIT {
            debug!(
                variant = fetch.variant,
                segment_index = seg_idx,
                current_segment_index = self.current_segment_index(),
                "committing fetch"
            );
        }

        self.coord.clear_pending_segment_request(SegmentRequest {
            segment_index: seg_idx,
            variant: fetch.variant,
            seek_epoch: fetch.seek_epoch,
        });
        self.commit_segment(fetch, is_variant_switch, is_midstream_switch);
    }

    /// Whether the downloader should pause because it is too far ahead
    /// of the reader (byte-based or segment-based backpressure).
    pub(crate) fn should_throttle(&self) -> bool {
        if self.coord.timeline().is_flushing() {
            return false;
        }

        let current_variant = self.abr.get_current_variant_index();
        if !self
            .segments
            .lock_sync()
            .is_segment_loaded(current_variant, self.current_segment_index())
        {
            return false;
        }

        if let Some(limit) = self.look_ahead_bytes {
            let reader_pos = self.coord.timeline().byte_position();
            let downloaded = self.segments.lock_sync().max_end_offset();
            if downloaded.saturating_sub(reader_pos) > limit {
                return true;
            }
        }

        if let Some(limit) = self.look_ahead_segments {
            let reader_seg = self.reader_segment_hint(current_variant);
            if self.current_segment_index() > reader_seg + limit {
                return true;
            }
        }

        false
    }
}
