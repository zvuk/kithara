use std::{sync::atomic::Ordering, time::Duration};

use kithara_abr::{AbrDecision, ThroughputSample, ThroughputSampleSource};
use kithara_events::{HlsEvent, SeekEpoch};
use kithara_platform::time::Instant;
use tracing::{debug, trace};

use super::{
    helpers::{first_missing_segment, is_cross_codec_switch, should_request_init},
    state::{HlsScheduler, MAX_LOG_PLANS},
};
use crate::{
    HlsError,
    coord::SegmentRequest,
    ids::{SegmentId, VariantIndex},
    playlist::PlaylistAccess,
};

/// Plan descriptor for a single HLS segment download.
pub(crate) struct HlsPlan {
    pub(crate) variant: VariantIndex,
    pub(crate) segment: SegmentId,
    pub(crate) need_init: bool,
    pub(crate) seek_epoch: SeekEpoch,
}

/// Outcome of a single planning pass.
pub(crate) enum PlanOutcome {
    /// One or more plans ready to fetch in parallel.
    Batch(Vec<HlsPlan>),
    /// Required metadata (e.g. size map) is missing. Caller must fetch it.
    FetchMetadata(VariantIndex),
    /// No new work this tick, but scheduler is not done.
    Idle,
    /// All variants drained — scheduler should exit.
    #[expect(
        dead_code,
        reason = "Phase 4c prep — will be used for final EOF transition"
    )]
    Complete,
    /// Single-step progress (unused by HLS, kept for worker loop contract).
    #[expect(dead_code, reason = "worker loop handles this variant defensively")]
    Step,
}

pub(crate) enum DemandOutcome {
    Fetch(HlsPlan),
    FetchMetadata(VariantIndex),
}

impl HlsScheduler {
    pub(super) fn plan_impl(&mut self) -> PlanOutcome {
        if self.coord.timeline().is_flushing() {
            return PlanOutcome::Idle;
        }

        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let variant = self.abr.get_current_variant_index();

        let Some(num_segments) = self.num_segments_for_plan(variant) else {
            return PlanOutcome::Idle;
        };

        self.publish_variant_applied(old_variant, variant, &decision);

        if self.handle_tail_state(variant, num_segments) {
            return PlanOutcome::Idle;
        }

        let (is_variant_switch, is_midstream_switch) =
            match self.ensure_variant_ready(variant, self.current_segment_index()) {
                Ok(flags) => flags,
                Err(outcome) => return outcome,
            };

        let old_variant_param = if old_variant != variant {
            Some(old_variant)
        } else {
            None
        };
        let is_cross_codec = old_variant_param
            .is_some_and(|ov| is_cross_codec_switch(&self.playlist_state, ov, variant));
        let (plans, batch_end) = self.build_batch_plans(
            variant,
            num_segments,
            is_variant_switch,
            is_midstream_switch,
            old_variant_param,
            is_cross_codec,
        );

        if plans.is_empty() {
            let advanced = batch_end > self.current_segment_index();
            self.advance_current_segment_index(batch_end);
            self.coord.condvar.notify_all();
            if advanced {
                self.coord.reader_advanced.notify_one();
            }
            return PlanOutcome::Idle;
        }

        PlanOutcome::Batch(plans)
    }

    pub(super) fn poll_demand_impl(&mut self) -> Option<DemandOutcome> {
        let req = self.next_valid_demand_request()?;
        trace!(
            variant = req.variant,
            segment_index = req.segment_index,
            "processing on-demand segment request"
        );

        let Some((req, num_segments)) = self.num_segments_for_demand(req) else {
            return Some(DemandOutcome::FetchMetadata(req.variant));
        };

        if Self::demand_request_out_of_range(&req, num_segments) {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        if self.is_below_switch_floor(req.variant, req.segment_index) {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                floor = self.gap_scan_start_segment(),
                "dropping demand below switched-layout floor"
            );
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        if self.segment_loaded_for_demand(
            req.variant,
            req.segment_index,
            "segment loaded at stale offset, refreshing demand request",
            "segment already loaded, skipping",
        ) {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        let (is_variant_switch, is_midstream_switch) =
            match self.prepare_variant_for_demand(req.variant, req.segment_index) {
                Some(Ok(flags)) => flags,
                Some(Err(outcome)) => return Some(outcome),
                None => {
                    self.coord.clear_pending_segment_request(req);
                    return None;
                }
            };

        if self.should_skip_pre_switch_variant(req.variant, req.segment_index, is_midstream_switch)
        {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        if self.segment_loaded_for_demand(
            req.variant,
            req.segment_index,
            "segment loaded with stale offset after metadata calc, refreshing",
            "segment loaded from cache after metadata calc",
        ) {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        Some(DemandOutcome::Fetch(
            self.build_demand_plan(&req, is_variant_switch),
        ))
    }

    pub(super) fn next_valid_demand_request(&mut self) -> Option<SegmentRequest> {
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

    fn num_segments_for_demand(&mut self, req: SegmentRequest) -> Option<(SegmentRequest, usize)> {
        if let Some(value) = self.num_segments(req.variant) {
            return Some((req, value));
        }

        self.publish_download_error(
            "missing variant in playlist state for demand",
            &HlsError::VariantNotFound(format!("variant {}", req.variant)),
        );
        self.coord.requeue_segment_request(req);
        self.coord.condvar.notify_all();
        None
    }

    fn demand_request_out_of_range(req: &SegmentRequest, num_segments: usize) -> bool {
        if req.segment_index < num_segments {
            return false;
        }
        debug!(
            variant = req.variant,
            segment_index = req.segment_index,
            num_segments,
            "segment index out of range"
        );
        true
    }

    pub(super) fn segment_loaded_for_demand(
        &self,
        variant: usize,
        segment_index: usize,
        _stale_reason: &str,
        loaded_reason: &str,
    ) -> bool {
        let seg_data = {
            let segments = self.segments.lock_sync();
            if variant != segments.layout_variant() || !segments.is_visible(variant, segment_index)
            {
                return false;
            }
            segments
                .variant_segments(variant)
                .and_then(|vs| vs.get(segment_index))
                .cloned()
        };
        let Some(data) = seg_data else {
            return false;
        };

        if !self.segment_resources_available(&data) {
            debug!(
                variant,
                segment_index, "segment metadata present but resources evicted, need re-download"
            );
            return false;
        }

        trace!(
            variant,
            segment_index,
            reason = loaded_reason,
            "demand segment already loaded"
        );
        true
    }

    fn prepare_variant_for_demand(
        &mut self,
        variant: usize,
        segment_index: usize,
    ) -> Option<Result<(bool, bool), DemandOutcome>> {
        match self.ensure_variant_ready(variant, segment_index) {
            Ok(flags) => Some(Ok(flags)),
            Err(PlanOutcome::FetchMetadata(v)) => Some(Err(DemandOutcome::FetchMetadata(v))),
            Err(_) => unreachable!(),
        }
    }

    fn should_skip_pre_switch_variant(
        &self,
        variant: usize,
        _segment_index: usize,
        is_midstream_switch: bool,
    ) -> bool {
        if !self.coord.had_midstream_switch.load(Ordering::Acquire) || is_midstream_switch {
            return false;
        }
        let layout = self.segments.lock_sync().layout_variant();
        if variant == layout {
            return false;
        }
        let current_variant = self.abr.get_current_variant_index();
        if variant == current_variant {
            return false;
        }
        debug!(
            variant,
            current_variant, "skipping stale segment from pre-switch variant"
        );
        true
    }

    pub(super) fn build_demand_plan(
        &mut self,
        req: &SegmentRequest,
        is_variant_switch: bool,
    ) -> HlsPlan {
        self.bus.publish(HlsEvent::SegmentStart {
            variant: req.variant,
            segment_index: req.segment_index,
            byte_offset: self.coord.timeline().download_position(),
        });

        let has_init = self.variant_has_init(req.variant);
        let need_init = has_init
            && (self.force_init_for_seek
                || should_request_init(
                    self.switch_needs_init(req.variant, req.segment_index, is_variant_switch),
                    SegmentId::Media(req.segment_index),
                )
                || self.demand_init_evicted(req.variant, req.segment_index));
        if need_init {
            self.force_init_for_seek = false;
        }

        HlsPlan {
            variant: req.variant,
            segment: SegmentId::Media(req.segment_index),
            need_init,
            seek_epoch: req.seek_epoch,
        }
    }

    pub(super) fn ensure_variant_ready(
        &mut self,
        variant: usize,
        segment_index: usize,
    ) -> Result<(bool, bool), PlanOutcome> {
        let (is_variant_switch, is_midstream_switch) =
            self.classify_variant_transition(variant, segment_index);
        self.handle_midstream_switch(is_midstream_switch);

        if !self.playlist_state.has_size_map(variant) {
            return Err(PlanOutcome::FetchMetadata(variant));
        }

        {
            let mut segments = self.segments.lock_sync();
            if let Some(sizes) = self.playlist_state.segment_sizes(variant) {
                segments.set_expected_sizes(variant, sizes);
            }
        }
        if is_variant_switch {
            self.download_variant = variant;
        }

        let (cached_count, cached_end_offset) = self.populate_cached_segments_if_needed(variant);
        self.apply_cached_segment_progress(variant, cached_count, cached_end_offset);

        Ok((is_variant_switch, is_midstream_switch))
    }

    pub(super) fn num_segments_for_plan(&mut self, variant: usize) -> Option<usize> {
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

    pub(super) fn handle_tail_state(&mut self, variant: usize, num_segments: usize) -> bool {
        if self.current_segment_index() < num_segments {
            return false;
        }

        let timeline_seek_epoch = self.coord.timeline().seek_epoch();
        if timeline_seek_epoch != self.active_seek_epoch {
            self.coord.timeline().set_eof(false);
            self.coord.condvar.notify_all();
            return true;
        }

        if self.coord.had_midstream_switch.load(Ordering::Acquire) {
            let rewind_variant = self.rewind_reference_variant(variant);
            if self.rewind_to_first_missing_segment(rewind_variant, num_segments) {
                return true;
            }
        }

        let current_variant = self.layout_variant();
        if current_variant != variant {
            let new_variant_empty = self
                .segments
                .lock_sync()
                .variant_segments(variant)
                .is_none_or(crate::stream_index::VariantSegments::is_empty);
            if new_variant_empty {
                debug!(
                    current_variant,
                    new_variant = variant,
                    num_segments,
                    "ABR variant switch in tail state (no segments yet); \
                     resetting cursor to segment 0"
                );
                self.reset_cursor(0);
                self.coord.condvar.notify_all();
                return false;
            }
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
        let missing_segment = {
            let segments = self.segments.lock_sync();
            first_missing_segment(
                &segments,
                variant,
                self.gap_scan_start_segment(),
                num_segments,
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

    pub(super) fn publish_variant_applied(
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

    pub(super) fn build_batch_plans(
        &mut self,
        variant: usize,
        num_segments: usize,
        is_variant_switch: bool,
        is_midstream_switch: bool,
        old_variant: Option<usize>,
        is_cross_codec: bool,
    ) -> (Vec<HlsPlan>, usize) {
        let reader_seg = self.reader_segment_hint(variant);
        let post_seek_probe_only = self.active_seek_epoch > 0
            && self.coord.timeline().seek_epoch() == self.active_seek_epoch
            && reader_seg == self.gap_scan_start_segment()
            && self.current_segment_index() > reader_seg;
        if post_seek_probe_only {
            return (Vec::new(), self.current_segment_index());
        }

        let mut batch_end = (self.current_segment_index() + self.prefetch_count).min(num_segments);
        if let Some(limit) = self.look_ahead_segments {
            batch_end = batch_end.min(reader_seg.saturating_add(limit).saturating_add(1));
        }
        let seek_epoch = self.coord.timeline().seek_epoch();
        if self.active_seek_epoch > 0 && seek_epoch == self.active_seek_epoch {
            batch_end = batch_end.min(self.current_segment_index().saturating_add(1));
        }
        let mut plans = Vec::new();
        let has_init = self.variant_has_init(variant);
        let mut need_init = has_init
            && (self.force_init_for_seek
                || self.switch_needs_init(
                    variant,
                    self.current_segment_index(),
                    is_variant_switch,
                ));

        for seg_idx in self.current_segment_index()..batch_end {
            if self.should_skip_planned_segment(
                variant,
                seg_idx,
                is_midstream_switch,
                old_variant,
                is_cross_codec,
            ) {
                continue;
            }

            self.bus.publish(HlsEvent::SegmentStart {
                variant,
                segment_index: seg_idx,
                byte_offset: self.coord.timeline().download_position(),
            });

            let segment = SegmentId::Media(seg_idx);
            let plan_need_init = has_init && should_request_init(need_init, segment);
            plans.push(HlsPlan {
                variant,
                segment,
                need_init: plan_need_init,
                seek_epoch,
            });
            if plan_need_init {
                self.force_init_for_seek = false;
            }
            need_init = false;
        }

        if !plans.is_empty() && plans.len() <= MAX_LOG_PLANS {
            let first = plans
                .first()
                .map_or(0, |p| p.segment.media_index().unwrap_or(0));
            let last = plans
                .last()
                .map_or(0, |p| p.segment.media_index().unwrap_or(0));
            debug!(
                variant,
                current_segment_index = self.current_segment_index(),
                batch_end,
                first_segment = first,
                last_segment = last,
                count = plans.len(),
                "built batch plans"
            );
        }

        (plans, batch_end)
    }

    pub(super) fn should_skip_planned_segment(
        &mut self,
        variant: usize,
        seg_idx: usize,
        is_midstream_switch: bool,
        old_variant: Option<usize>,
        is_cross_codec: bool,
    ) -> bool {
        if let Some(old_var) = old_variant
            && !is_cross_codec
        {
            let old_data = {
                let segments = self.segments.lock_sync();
                if segments.is_segment_loaded(old_var, seg_idx) {
                    segments
                        .variant_segments(old_var)
                        .and_then(|vs| vs.get(seg_idx))
                        .cloned()
                } else {
                    None
                }
            };
            if let Some(data) = old_data
                && self.segment_resources_available(&data)
            {
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

        let current = self.abr.get_current_variant_index();
        variant != current
    }

    pub(super) fn make_abr_decision(&mut self) -> AbrDecision {
        if self.abr.is_locked() && !self.coord.timeline().is_seek_pending() {
            self.abr.unlock();
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
