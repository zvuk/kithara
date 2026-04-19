use std::{sync::atomic::Ordering, time::Duration};

use kithara_abr::{AbrDecision, ThroughputSample, ThroughputSampleSource};
use kithara_events::HlsEvent;
use kithara_platform::time::Instant;
use tracing::debug;

use super::{
    helpers::{first_missing_segment, is_cross_codec_switch},
    state::HlsScheduler,
};
use crate::{HlsError, coord::SegmentRequest};

impl HlsScheduler {
    pub(crate) fn next_valid_demand_request(&mut self) -> Option<SegmentRequest> {
        loop {
            let req = self.coord.take_segment_request()?;
            let current_epoch = self.coord.timeline().seek_epoch();
            if req.seek_epoch == current_epoch {
                if req.seek_epoch != self.active_seek_epoch {
                    self.reset_for_seek_epoch(req.seek_epoch, req.variant, req.segment_index);
                }
                return Some(req);
            }

            debug!(
                req_epoch = req.seek_epoch,
                current_epoch,
                variant = req.variant,
                segment_index = req.segment_index,
                "dropping stale on-demand request"
            );
            self.bus.publish(HlsEvent::StaleRequestDropped {
                seek_epoch: req.seek_epoch,
                current_epoch,
                variant: req.variant,
                segment_index: req.segment_index,
            });
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
        }
    }

    pub(crate) fn num_segments_for_plan(&mut self, variant: usize) -> Option<usize> {
        if let Some(value) = self.num_segments(variant) {
            return Some(value);
        }

        self.publish_download_error(
            "missing variant in playlist state",
            &HlsError::VariantNotFound(format!("variant {variant}")),
        );
        self.coord.condvar.notify_all();
        None
    }

    pub(crate) fn handle_tail_state(&mut self, variant: usize, num_segments: usize) -> bool {
        if self.current_segment_index() < num_segments {
            return false;
        }

        let timeline_seek_epoch = self.coord.timeline().seek_epoch();
        if timeline_seek_epoch != self.active_seek_epoch {
            self.coord.timeline().set_eof(false);
            self.coord.condvar.notify_all();
            return false;
        }

        // Always check for invalidated/missing segments at tail — not just
        // after midstream switches. LRU eviction or DRM re-processing can
        // invalidate committed segments behind the cursor.
        {
            let rewind_variant = if self.coord.had_midstream_switch.load(Ordering::Acquire) {
                self.rewind_reference_variant(variant)
            } else {
                variant
            };
            if self.rewind_to_first_missing_segment(rewind_variant, num_segments) {
                return false;
            }
        }

        let layout = self.layout_variant();
        if layout != variant {
            let new_variant_empty = self
                .segments
                .lock_sync()
                .variant_segments(variant)
                .is_none_or(crate::stream_index::VariantSegments::is_empty);
            if new_variant_empty {
                debug!(
                    layout,
                    new_variant = variant,
                    num_segments,
                    "ABR variant switch in tail state (no segments yet); \
                     resetting cursor to segment 0"
                );
                self.reset_cursor(0);
                self.coord.condvar.notify_all();
                return false;
            }

            // ABR variant done, but reader is still on the old layout
            // variant. Check if the layout variant has gaps that need
            // filling so the reader can make progress. Clamp the scan by
            // reader position so we don't backfill segments the reader has
            // already passed (see `rewind_to_first_missing_segment`).
            let layout_num = self.num_segments(layout).unwrap_or(0);
            let scan_start = self.reader_segment_floor();
            let layout_gap = {
                let segs = self.segments.lock_sync();
                first_missing_segment(
                    &segs,
                    layout,
                    scan_start,
                    layout_num,
                    self.backend.is_ephemeral(),
                )
            };
            if let Some(gap_seg) = layout_gap {
                debug!(
                    layout,
                    gap_seg, variant, "tail: ABR variant done, filling layout variant gap"
                );
                self.filling_layout_gap = true;
                self.download_variant = layout;
                self.reset_cursor(gap_seg);
                self.coord.condvar.notify_all();
                return false;
            }
            self.filling_layout_gap = false;
        } else {
            self.filling_layout_gap = false;
        }

        let stream_end = self.effective_total_bytes().unwrap_or(0);
        let playback_at_end =
            stream_end == 0 || self.coord.timeline().byte_position() >= stream_end;
        if !playback_at_end {
            self.coord.timeline().set_eof(false);
            self.coord.condvar.notify_all();
            return true;
        }

        if !self.coord.timeline().eof() {
            debug!("reached end of playlist");
            self.coord.timeline().set_eof(true);
            self.bus.publish(HlsEvent::EndOfStream);
        }
        self.coord.condvar.notify_all();
        true
    }

    fn rewind_reference_variant(&self, fallback_variant: usize) -> usize {
        let current_variant = self.abr.get_current_variant_index();
        if current_variant < self.playlist_state.num_variants() {
            current_variant
        } else {
            fallback_variant
        }
    }

    fn rewind_to_first_missing_segment(&mut self, variant: usize, num_segments: usize) -> bool {
        // Clamp gap scan by reader position: segments strictly behind the
        // reader will not be replayed, and in ephemeral LRU stores those
        // "missing" slots are evictions by design — re-fetching them only
        // evicts the tail segments the reader is currently reading.
        let scan_start = self
            .gap_scan_start_segment()
            .max(self.reader_segment_floor());
        let missing_segment = {
            let segments = self.segments.lock_sync();
            first_missing_segment(
                &segments,
                variant,
                scan_start,
                num_segments,
                self.backend.is_ephemeral(),
            )
        };
        let Some(segment_index) = missing_segment else {
            return false;
        };

        debug!(
            variant,
            segment_index,
            num_segments,
            "playlist tail reached with gaps; rewinding to first missing segment"
        );
        self.rewind_current_segment_index(segment_index);
        self.coord.condvar.notify_all();
        true
    }

    pub(crate) fn publish_variant_applied(
        &self,
        old_variant: usize,
        variant: usize,
        decision: &AbrDecision,
    ) {
        if !decision.changed {
            return;
        }
        debug!(
            from = old_variant,
            to = variant,
            reason = ?decision.reason,
            "publishing VariantApplied event"
        );
        self.bus.publish(HlsEvent::VariantApplied {
            from_variant: old_variant,
            to_variant: variant,
            reason: decision.reason,
        });
    }

    pub(crate) fn should_skip_planned_segment(
        &mut self,
        variant: usize,
        seg_idx: usize,
        is_midstream_switch: bool,
        old_variant: Option<usize>,
    ) -> bool {
        // If the old variant already has this segment loaded with
        // available resources, skip the download — the reader will
        // demand the correct variant's data on-demand when it needs it.
        // Do NOT copy old-variant SegmentData (wrong media_url) into
        // the new variant's StreamIndex.
        if let Some(old_var) = old_variant {
            let old_loaded = {
                let segments = self.segments.lock_sync();
                segments.is_segment_loaded(old_var, seg_idx)
                    && segments
                        .variant_segments(old_var)
                        .and_then(|vs| vs.get(seg_idx))
                        .is_some_and(|data| self.segment_resources_available(data))
            };
            if old_loaded {
                self.advance_current_segment_index(seg_idx + 1);
                return true;
            }
        }

        let seg_data = {
            let segments = self.segments.lock_sync();
            segments
                .variant_segments(variant)
                .and_then(|vs| vs.get(seg_idx))
                .cloned()
        };
        if let Some(data) = seg_data {
            if self.segment_resources_available(&data) {
                self.advance_current_segment_index(seg_idx + 1);
                return true;
            }
            debug!(
                variant,
                segment_index = seg_idx,
                "segment in plan window lost resources, forcing refresh"
            );
            return false;
        }

        let had_switch = self.coord.had_midstream_switch.load(Ordering::Acquire);
        if !had_switch || is_midstream_switch {
            return false;
        }

        // Don't skip if we are intentionally downloading this variant
        // (e.g., layout gap-fill sets download_variant to the layout
        // variant even though ABR moved on).
        if self.download_variant == variant {
            return false;
        }

        let current = self.abr.get_current_variant_index();
        variant != current
    }

    pub(crate) fn make_abr_decision(&mut self) -> AbrDecision {
        // Keep ABR locked for the entire pending-seek window. Without
        // this, `decide()` can fire between `Timeline::initiate_seek`
        // and the peer reaching `reset_for_seek_epoch` (where the
        // existing lock call lives) and switch variants mid-seek — the
        // anchor the source just resolved for the layout variant then
        // points at segments the downloader no longer plans to fetch,
        // and `source_is_ready_for_apply_seek` stays `Waiting` forever.
        // The lock is refcounted, so pairing it with the unlock below
        // keeps the count at 0/1 across the seek lifetime.
        let seek_pending = self.coord.timeline().is_seek_pending();
        match (self.abr.is_locked(), seek_pending) {
            (false, true) => self.abr.lock(),
            (true, false) => self.abr.unlock(),
            _ => {}
        }

        let now = Instant::now();
        let current_variant = self.abr.get_current_variant_index();
        let decision = self.abr.decide(now);

        if decision.changed {
            let cross_codec = is_cross_codec_switch(
                &self.playlist_state,
                current_variant,
                decision.target_variant_index,
            );
            debug!(
                from = current_variant,
                to = decision.target_variant_index,
                cross_codec,
                reason = ?decision.reason,
                "ABR variant switch"
            );
            self.abr.apply(&decision, now);
        }

        decision
    }

    pub(super) fn record_throughput(
        &mut self,
        bytes: u64,
        duration: Duration,
        content_duration: Option<Duration>,
    ) {
        let min_ms = self.abr.min_throughput_record_ms();
        if duration.as_millis() < min_ms {
            return;
        }

        let sample = ThroughputSample {
            bytes,
            duration,
            at: Instant::now(),
            source: ThroughputSampleSource::Network,
            content_duration,
        };

        self.abr.push_sample(sample);

        #[expect(
            clippy::cast_precision_loss,
            reason = "throughput estimation tolerates f64 precision"
        )]
        let bytes_per_second = if duration.as_secs_f64() > 0.0 {
            bytes as f64 / duration.as_secs_f64()
        } else {
            0.0
        };
        self.bus
            .publish(HlsEvent::ThroughputSample { bytes_per_second });
    }
}
