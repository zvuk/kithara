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
    pub(crate) fn commit_fetch_inline(
        &mut self,
        variant: VariantIndex,
        seg_idx: usize,
        seek_epoch: kithara_events::SeekEpoch,
        media: crate::loading::SegmentMeta,
        init_len: u64,
        init_url: Option<url::Url>,
        duration: std::time::Duration,
    ) {
        let current_epoch = self.coord.timeline().seek_epoch();

        if is_stale_epoch(seek_epoch, current_epoch) {
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
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant,
                seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_stale_cross_codec(variant, seg_idx) {
            debug!(
                variant,
                segment_index = seg_idx,
                current_variant = self.abr.get_current_variant_index(),
                "dropping stale cross-codec fetch after switched anchor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant,
                seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_below_switch_floor(variant, seg_idx) {
            debug!(
                variant,
                segment_index = seg_idx,
                floor = self.gap_scan_start_segment(),
                "dropping fetch below switched-layout floor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant,
                seek_epoch,
            });
            self.coord.condvar.notify_all();
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
