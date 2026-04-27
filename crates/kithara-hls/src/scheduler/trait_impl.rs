use std::sync::atomic::Ordering;

use kithara_events::HlsEvent;
use tracing::{debug, trace};

use super::{
    helpers::is_stale_epoch,
    state::{HlsScheduler, VERBOSE_SEGMENT_LIMIT},
};
use crate::{coord::SegmentRequest, ids::VariantIndex};

impl Drop for HlsScheduler {
    fn drop(&mut self) {
        self.coord.stopped.store(true, Ordering::Release);
        self.coord.condvar.notify_all();
    }
}

impl HlsScheduler {
    /// Commit a completed fetch with individual arguments.
    ///
    /// Used by `HlsPeer::on_complete` — the Downloader-driven path.
    #[expect(
        clippy::too_many_arguments,
        reason = "segment commit requires all metadata"
    )]
    pub(crate) fn commit_fetch_inline(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
        media: &crate::loading::SegmentMeta,
        init_len: u64,
        init_url: Option<url::Url>,
        duration: std::time::Duration,
    ) {
        if self.reject_stale_fetch(variant, seg_idx, seek_epoch) {
            return;
        }

        if init_len > 0 {
            self.sent_init_for_variant.insert(variant);
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

        self.coord.clear_pending_segment_request(SegmentRequest {
            segment_index: seg_idx,
            variant,
            seek_epoch,
        });

        self.commit_segment(variant, seg_idx, media, init_len, init_url, duration);
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

    fn drop_pending(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
    ) {
        self.coord.clear_pending_segment_request(SegmentRequest {
            segment_index: seg_idx,
            variant,
            seek_epoch,
        });
        self.coord.condvar.notify_all();
    }
}
