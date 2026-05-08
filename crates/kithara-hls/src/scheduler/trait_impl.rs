use std::{sync::atomic::Ordering, time::Duration};

use kithara_events::HlsEvent;
use tracing::{debug, trace};
use url::Url;

use super::{
    helpers::is_stale_epoch,
    state::{HlsScheduler, VERBOSE_SEGMENT_LIMIT},
};
use crate::{coord::SegmentRequest, ids::VariantIndex, loading::SegmentMeta};

impl Drop for HlsScheduler {
    fn drop(&mut self) {
        self.coord.stopped.store(true, Ordering::Release);
        self.coord.condvar.notify_all();
    }
}

/// Outcome of a completed segment fetch ready to be committed: the media
/// body, the optional init segment, and how long the fetch took.
///
/// Produced where the fetch finishes (`HlsPeer::on_complete`, the cached
/// commit path) and consumed by [`HlsScheduler::commit_fetch_inline`].
pub(crate) struct LoadedSegmentBody<'a> {
    pub(crate) media: &'a SegmentMeta,
    pub(crate) duration: Duration,
    pub(crate) init_url: Option<Url>,
    pub(crate) init_len: u64,
}

impl HlsScheduler {
    /// Commit a completed fetch into the scheduler's segment index and
    /// clear the matching pending demand.
    ///
    /// Used by `HlsPeer::on_complete` — the Downloader-driven path.
    pub(crate) fn commit_fetch_inline(
        &mut self,
        request: SegmentRequest,
        body: LoadedSegmentBody<'_>,
    ) {
        let SegmentRequest {
            variant,
            seek_epoch,
            segment_index: seg_idx,
        } = request;
        let LoadedSegmentBody {
            media,
            init_len,
            init_url,
            duration,
        } = body;
        if self.reject_stale_fetch(variant, seg_idx, seek_epoch) {
            return;
        }

        if init_len > 0 {
            self.runtime.sent_init_for_variant.insert(variant);
        }

        if seg_idx == self.current_segment_index() {
            self.advance_current_segment_index(seg_idx + 1);
        }

        if seg_idx <= VERBOSE_SEGMENT_LIMIT {
            debug!(
                variant,
                segment_index = seg_idx,
                current_segment_index = self.current_segment_index(),
                "committing fetch"
            );
        }

        self.coord.clear_pending_segment_request(request);

        self.commit_segment(variant, seg_idx, media, init_len, init_url, duration);
    }

    fn drop_pending(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
    ) {
        self.coord.clear_pending_segment_request(SegmentRequest {
            variant,
            seek_epoch,
            segment_index: seg_idx,
        });
        self.coord.condvar.notify_all();
    }

    fn reject_if_stale_epoch(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
    ) -> bool {
        let current_epoch = self.coord.timeline().seek_epoch();
        if !is_stale_epoch(seek_epoch, current_epoch) {
            return false;
        }
        trace!(
            fetch_epoch = seek_epoch,
            current_epoch,
            variant,
            segment_index = seg_idx,
            "dropping stale fetch before commit"
        );
        self.bus.publish(HlsEvent::StaleFetchDropped {
            seek_epoch,
            current_epoch,
            variant,
            segment_index: seg_idx,
        });
        self.drop_pending(variant, seg_idx, seek_epoch);
        true
    }

    fn reject_if_stale_geometry(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
    ) -> bool {
        if self.is_stale_cross_codec(variant, seg_idx) {
            debug!(
                variant,
                segment_index = seg_idx,
                current_variant = self.abr.current_variant_index(),
                "dropping stale cross-codec fetch after switched anchor"
            );
            self.drop_pending(variant, seg_idx, seek_epoch);
            return true;
        }

        if self.is_below_switch_floor(variant, seg_idx) {
            debug!(
                variant,
                segment_index = seg_idx,
                floor = self.gap_scan_start_segment(),
                "dropping fetch below switched-layout floor"
            );
            self.drop_pending(variant, seg_idx, seek_epoch);
            return true;
        }

        false
    }

    /// Returns true if the fetch is stale and was dropped.
    fn reject_stale_fetch(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
    ) -> bool {
        if self.reject_if_stale_epoch(variant, seg_idx, seek_epoch) {
            return true;
        }
        self.reject_if_stale_geometry(variant, seg_idx, seek_epoch)
    }
}
