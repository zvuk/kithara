#![forbid(unsafe_code)]

use tracing::debug;

use super::*;

#[derive(Default)]
pub(super) struct WaitRangeState {
    metadata_miss_count: usize,
    on_demand_request_epoch: Option<u64>,
}

impl WaitRangeState {
    pub(super) fn reset_for_seek_epoch(&mut self, seek_epoch: u64) {
        if self
            .on_demand_request_epoch
            .is_some_and(|epoch| epoch != seek_epoch)
        {
            self.on_demand_request_epoch = None;
        }
    }

    pub(super) fn on_demand_pending(&self, seek_epoch: u64) -> bool {
        self.on_demand_request_epoch
            .is_some_and(|epoch| epoch == seek_epoch)
    }

    pub(super) fn mark_on_demand_requested(&mut self, seek_epoch: u64) {
        self.on_demand_request_epoch = Some(seek_epoch);
    }

    pub(super) fn clear_on_demand_requested(&mut self) {
        self.on_demand_request_epoch = None;
    }
}

pub(super) struct WaitRangeContext {
    pub(super) effective_total: u64,
    pub(super) eof: bool,
    pub(super) expected_total: u64,
    pub(super) num_entries: usize,
    pub(super) range_ready: bool,
    pub(super) seek_epoch: u64,
    pub(super) total: u64,
}

pub(super) enum WaitRangeDecision {
    Cancelled,
    Continue,
    Eof,
    Interrupted,
    Ready,
}

impl HlsSource {
    pub(super) fn fallback_segment_index_for_offset(
        &self,
        variant: usize,
        offset: u64,
    ) -> Option<usize> {
        let num_segments = self.playlist_state.num_segments(variant)?;
        if num_segments == 0 {
            return None;
        }

        if let Some(total) = self.shared.timeline.total_bytes()
            && total > 0
        {
            let estimate = ((u128::from(offset) * num_segments as u128) / u128::from(total))
                .min((num_segments - 1) as u128);
            return Some(estimate as usize);
        }

        let hinted = self.shared.current_segment_index.load(Ordering::Acquire) as usize;
        Some(hinted.min(num_segments - 1))
    }

    pub(super) fn build_wait_range_context(
        &self,
        segments: &DownloadState,
        range: &Range<u64>,
    ) -> WaitRangeContext {
        let seek_epoch = self.shared.timeline.seek_epoch();
        let range_ready = self.range_ready_from_segments(segments, range);
        let eof = self.shared.timeline.eof();
        let total = segments.max_end_offset();
        let expected_total = self.shared.timeline.total_bytes().unwrap_or(0);
        let effective_total = total.max(expected_total);
        let num_entries = segments.num_entries();

        WaitRangeContext {
            effective_total,
            eof,
            expected_total,
            num_entries,
            range_ready,
            seek_epoch,
            total,
        }
    }

    pub(super) fn decide_wait_range(
        &self,
        range: &Range<u64>,
        context: &WaitRangeContext,
    ) -> WaitRangeDecision {
        if self.shared.cancel.is_cancelled() {
            return WaitRangeDecision::Cancelled;
        }

        if self.shared.stopped.load(Ordering::Acquire) && !context.range_ready {
            if context.eof && context.effective_total > 0 && range.start >= context.effective_total
            {
                return WaitRangeDecision::Eof;
            }
            return WaitRangeDecision::Cancelled;
        }

        if context.range_ready {
            return WaitRangeDecision::Ready;
        }

        if self.shared.timeline.is_flushing() {
            return WaitRangeDecision::Interrupted;
        }

        if context.eof && context.effective_total > 0 && range.start >= context.effective_total {
            debug!(
                range_start = range.start,
                total_bytes = context.total,
                expected_total_length = context.expected_total,
                effective_total_length = context.effective_total,
                num_entries = context.num_entries,
                had_midstream_switch = self.shared.had_midstream_switch.load(Ordering::Acquire),
                "wait_range: EOF"
            );
            return WaitRangeDecision::Eof;
        }

        WaitRangeDecision::Continue
    }

    #[expect(
        clippy::result_large_err,
        reason = "Stream source APIs use StreamResult<_, HlsError> consistently"
    )]
    pub(super) fn request_on_demand_if_needed(
        &self,
        range_start: u64,
        seek_epoch: u64,
        state: &mut WaitRangeState,
    ) -> StreamResult<(), HlsError> {
        if state.on_demand_pending(seek_epoch) {
            self.shared.reader_advanced.notify_one();
            return Ok(());
        }

        let requested = self.request_on_demand_segment(
            range_start,
            seek_epoch,
            &mut state.metadata_miss_count,
            WAIT_RANGE_MAX_METADATA_MISS_SPINS,
        )?;
        if requested {
            state.mark_on_demand_requested(seek_epoch);
        }
        Ok(())
    }

    #[expect(
        clippy::result_large_err,
        reason = "Stream source APIs use StreamResult<_, HlsError> consistently"
    )]
    pub(super) fn request_on_demand_segment(
        &self,
        range_start: u64,
        seek_epoch: u64,
        metadata_miss_count: &mut usize,
        max_metadata_miss_spins: usize,
    ) -> StreamResult<bool, HlsError> {
        let current_variant = self.resolve_current_variant();
        if let Some(segment_index) = self
            .playlist_state
            .find_segment_at_offset(current_variant, range_start)
        {
            *metadata_miss_count = 0;
            debug!(
                variant = current_variant,
                segment_index,
                offset = range_start,
                "wait_range: requesting on-demand segment load"
            );
            self.push_segment_request(current_variant, segment_index, seek_epoch);
            return Ok(true);
        }

        if let Some(segment_index) =
            self.fallback_segment_index_for_offset(current_variant, range_start)
        {
            *metadata_miss_count = 0;
            let hint_index = self.shared.current_segment_index.load(Ordering::Acquire) as usize;
            debug!(
                variant = current_variant,
                segment_index,
                offset = range_start,
                "wait_range: metadata miss fallback segment load"
            );
            self.bus.publish(HlsEvent::Seek {
                stage: "wait_range_metadata_fallback",
                seek_epoch,
                variant: current_variant,
                offset: range_start,
                from_segment_index: hint_index,
                to_segment_index: segment_index,
            });
            self.push_segment_request(current_variant, segment_index, seek_epoch);
            return Ok(true);
        }

        *metadata_miss_count = metadata_miss_count.saturating_add(1);
        debug!(
            offset = range_start,
            variant = current_variant,
            seek_epoch,
            metadata_miss_count = *metadata_miss_count,
            "wait_range: no metadata to find segment"
        );
        self.bus.publish(HlsEvent::SeekMetadataMiss {
            seek_epoch,
            offset: range_start,
            variant: current_variant,
        });
        self.shared.reader_advanced.notify_one();

        if *metadata_miss_count >= max_metadata_miss_spins {
            let error = format!(
                "seek metadata miss: offset={} variant={} epoch={} misses={}",
                range_start, current_variant, seek_epoch, *metadata_miss_count
            );
            self.bus.publish(HlsEvent::Error {
                error: error.clone(),
                recoverable: false,
            });
            return Err(StreamError::Source(HlsError::SegmentNotFound(error)));
        }

        Ok(false)
    }
}
