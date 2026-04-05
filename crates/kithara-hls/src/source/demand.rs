use std::sync::atomic::Ordering;

use tracing::{debug, trace};

use super::{
    core::HlsSource,
    types::{DemandRequestOutcome, MetadataMissReason, ResolvedSegment},
};
use crate::{
    coord::SegmentRequest,
    ids::{SegmentIndex, VariantIndex},
    playlist::PlaylistAccess,
};

/// Bit shift for packing variant index into the high 16 bits of a u64 key.
const VARIANT_SHIFT: u64 = 48;
/// Mask for the lower 48-bit offset portion of a packed variant+offset key.
const OFFSET_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

impl HlsSource {
    pub(super) fn push_segment_request(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        seek_epoch: u64,
    ) -> bool {
        self.coord.enqueue_segment_request(SegmentRequest {
            segment_index,
            variant,
            seek_epoch,
        })
    }

    pub(super) fn resolve_demand_variant(&self) -> VariantIndex {
        self.resolve_current_variant()
    }

    pub(super) fn queue_segment_request_for_offset(
        &self,
        range_start: u64,
        seek_epoch: u64,
    ) -> bool {
        // Always use layout_variant for demand — ABR is invisible here.
        if let Some((variant, segment_index)) = self.layout_segment_for_offset(range_start) {
            return self.push_segment_request(variant, segment_index, seek_epoch);
        }

        let variant = self.resolve_demand_variant();
        if let Some(resolved) = self.resolve_segment_for_offset(range_start, variant) {
            let segment_index = Self::segment_index(resolved);
            return self.push_segment_request(variant, segment_index, seek_epoch);
        }

        false
    }

    pub(super) fn resolve_segment_for_offset(
        &self,
        range_start: u64,
        variant: VariantIndex,
    ) -> Option<ResolvedSegment> {
        if let Some(segment_index) = self.committed_segment_for_offset(range_start, variant) {
            return Some(ResolvedSegment::Committed(segment_index));
        }

        // Try playlist metadata for layout-based resolution
        if let Some(segment_index) = self
            .playlist_state
            .find_segment_at_offset(variant, range_start)
        {
            return Some(ResolvedSegment::Layout(segment_index));
        }

        self.fallback_segment_index_for_offset(variant, range_start)
            .map(ResolvedSegment::Fallback)
    }

    pub(super) fn segment_index(resolved: ResolvedSegment) -> SegmentIndex {
        match resolved {
            ResolvedSegment::Committed(segment_index)
            | ResolvedSegment::Layout(segment_index)
            | ResolvedSegment::Fallback(segment_index) => segment_index,
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "segments lock must be held for both is_segment_loaded and range_ready_from_segments"
    )]
    pub(super) fn loaded_segment_ready(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> bool {
        let segments = self.segments.lock_sync();
        if !segments.is_visible(variant, segment_index) {
            return false;
        }
        segments
            .range_for(variant, segment_index)
            .is_some_and(|range| self.range_ready_from_segments(&segments, &range))
    }

    #[expect(
        clippy::result_large_err,
        reason = "Stream source APIs use StreamResult<_, HlsError> consistently"
    )]
    pub(super) fn request_on_demand_segment(
        &self,
        range_start: u64,
        seek_epoch: u64,
    ) -> DemandRequestOutcome {
        // Always use layout_variant for demand — ABR is invisible to source.
        if let Some((variant, segment_index)) = self.layout_segment_for_offset(range_start) {
            trace!(
                variant,
                segment_index,
                offset = range_start,
                "wait_range: layout on-demand segment load"
            );
            return DemandRequestOutcome::Requested {
                queued: self.push_segment_request(variant, segment_index, seek_epoch),
            };
        }

        let current_variant = self.resolve_demand_variant();
        if current_variant >= self.playlist_state.num_variants() {
            if self.is_transient_demand_gap() {
                self.coord.reader_advanced.notify_one();
                return DemandRequestOutcome::TransientGap;
            }

            return DemandRequestOutcome::MetadataMiss {
                variant: current_variant,
                reason: MetadataMissReason::VariantOutOfRange,
            };
        }

        if let Some(resolved) = self.resolve_segment_for_offset(range_start, current_variant) {
            let segment_index = Self::segment_index(resolved);
            self.log_resolved_demand_segment(range_start, seek_epoch, current_variant, resolved);
            return DemandRequestOutcome::Requested {
                queued: self.push_segment_request(current_variant, segment_index, seek_epoch),
            };
        }

        debug!(
            offset = range_start,
            variant = current_variant,
            seek_epoch,
            "wait_range: no metadata to find segment"
        );
        if self.is_transient_demand_gap() {
            self.coord.reader_advanced.notify_one();
            return DemandRequestOutcome::TransientGap;
        }

        DemandRequestOutcome::MetadataMiss {
            variant: current_variant,
            reason: MetadataMissReason::UnresolvedOffset,
        }
    }

    pub(super) fn log_resolved_demand_segment(
        &self,
        range_start: u64,
        seek_epoch: u64,
        current_variant: VariantIndex,
        resolved: ResolvedSegment,
    ) -> SegmentIndex {
        let segment_index = Self::segment_index(resolved);
        match resolved {
            ResolvedSegment::Committed(_) => {
                self.last_fallback_key.store(u64::MAX, Ordering::Relaxed);
                trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: committed on-demand segment load"
                );
            }
            ResolvedSegment::Layout(_) => {
                self.last_fallback_key.store(u64::MAX, Ordering::Relaxed);
                trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: layout on-demand segment load"
                );
            }
            ResolvedSegment::Fallback(_) => {
                let hint_index = self.current_segment_index().unwrap_or(0);
                trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: metadata miss fallback segment load"
                );
                let key = ((current_variant as u64) << VARIANT_SHIFT) | (range_start & OFFSET_MASK);
                let prev = self.last_fallback_key.swap(key, Ordering::Relaxed);
                if prev != key {
                    self.bus.publish(kithara_events::HlsEvent::Seek {
                        stage: "wait_range_metadata_fallback",
                        seek_epoch,
                        variant: current_variant,
                        offset: range_start,
                        from_segment_index: hint_index,
                        to_segment_index: segment_index,
                    });
                }
            }
        }
        segment_index
    }
}
