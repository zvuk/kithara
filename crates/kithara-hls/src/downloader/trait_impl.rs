use std::sync::{Arc, atomic::Ordering};

use kithara_events::HlsEvent;
use kithara_platform::BoxFuture;
use kithara_stream::{Downloader, PlanOutcome};
use tracing::{debug, trace};

use super::{
    helpers::is_stale_epoch,
    io::{HlsFetch, HlsIo, HlsPlan},
    state::{HlsDownloader, VERBOSE_SEGMENT_LIMIT},
};
use crate::{HlsError, coord::SegmentRequest};

impl Drop for HlsDownloader {
    fn drop(&mut self) {
        self.coord.stopped.store(true, Ordering::Release);
        self.coord.condvar.notify_all();
    }
}

impl Downloader for HlsDownloader {
    type Plan = HlsPlan;
    type Fetch = HlsFetch;
    type Error = HlsError;
    type Io = HlsIo;

    fn io(&self) -> &Self::Io {
        &self.io
    }

    fn poll_demand(&mut self) -> BoxFuture<'_, Option<HlsPlan>> {
        Box::pin(async move { self.poll_demand_impl().await })
    }

    fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<HlsPlan>> {
        Box::pin(async move { self.plan_impl().await })
    }

    fn commit(&mut self, fetch: HlsFetch) {
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

    fn should_throttle(&self) -> bool {
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

    fn wait_ready(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.coord.timeline().is_flushing() || !self.should_throttle() {
                return;
            }
            self.coord.notified_reader_advanced().await;
        })
    }

    fn demand_signal(&self) -> BoxFuture<'static, ()> {
        let coord = Arc::clone(&self.coord);
        Box::pin(async move {
            coord.notified_reader_advanced().await;
        })
    }
}
