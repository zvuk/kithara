#![forbid(unsafe_code)]

use tracing::debug;

use super::*;

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

    /// Check if the read position is past the effective end of stream.
    pub(super) fn is_past_eof(&self, segments: &DownloadState, range: &Range<u64>) -> bool {
        if !self.shared.timeline.eof() {
            return false;
        }
        let total = segments.max_end_offset();
        let expected_total = self.shared.timeline.total_bytes().unwrap_or(0);
        let effective_total = total.max(expected_total);
        effective_total > 0 && range.start >= effective_total
    }

    /// Check committed `DownloadState` for the segment covering `range_start`.
    /// Returns `Some(segment_index)` if a request should be issued.
    fn committed_segment_for_offset(&self, range_start: u64, variant: usize) -> Option<usize> {
        let segments = self.shared.segments.lock_sync();

        if let Some(seg) = segments.find_at_offset(range_start) {
            return Some(seg.segment_index);
        }

        if let Some(last) = segments.last_of_variant(variant)
            && range_start >= last.end_offset()
        {
            let next_idx = last.segment_index + 1;
            drop(segments);
            if let Some(num) = self.playlist_state.num_segments(variant)
                && next_idx < num
            {
                return Some(next_idx);
            }
            return None;
        }

        None
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

        // Step 1: Check committed data — authoritative for DRM-reconciled offsets.
        if let Some(idx) = self.committed_segment_for_offset(range_start, current_variant) {
            *metadata_miss_count = 0;
            debug!(
                variant = current_variant,
                segment_index = idx,
                offset = range_start,
                "wait_range: committed on-demand segment load"
            );
            self.push_segment_request(current_variant, idx, seek_epoch);
            return Ok(true);
        }

        // Step 2: Fall back to metadata lookup.
        // Safe for empty `DownloadState` (after Reset seek): reader position was
        // also set from metadata in `seek_time_anchor`, so same-space lookup.
        if let Some(segment_index) = self
            .playlist_state
            .find_segment_at_offset(current_variant, range_start)
        {
            *metadata_miss_count = 0;
            debug!(
                variant = current_variant,
                segment_index,
                offset = range_start,
                "wait_range: metadata on-demand segment load"
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
