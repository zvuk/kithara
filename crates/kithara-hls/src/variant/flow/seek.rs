use std::sync::atomic::Ordering;

use kithara_platform::time::Duration;
use kithara_stream::{SourceSeekAnchor, StreamError, StreamResult, needs_exact_byte_sizes};

use super::{HlsVariant, seqlock::AliasSnapshot, size::ExactSeekDemand};
use crate::HlsError;

#[derive(Clone, Copy)]
pub(crate) struct ResolvedSeekProjection {
    demand_generation: Option<u64>,
    alias_generation: u64,
    exact_anchor: u64,
}

impl ResolvedSeekProjection {
    pub(crate) const fn exact_anchor(self) -> u64 {
        self.exact_anchor
    }
}

impl HlsVariant {
    fn clear_segment_aware_seek_tail(&self) {
        self.seek
            .segment_aware_tail
            .store(Self::NO_SEEK_TAIL, Ordering::Release);
    }

    /// Resolve a time-target seek to its byte anchor on the produce core:
    /// lock-free layout reads plus atomic anchor stores. The fetch plan is
    /// owned by the download peer, which re-aims the queue when it observes
    /// the epoch bump (`seek_epoch_reset`); stale entries of the previous
    /// epoch are dropped by the dispatcher's epoch tag.
    pub(crate) fn prepare_seek_time_anchor(
        &self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>> {
        let variant = self.variant;
        let Some((seg_idx_u32, segment_start, segment_end)) = self.seek_point_at_time(position)
        else {
            return Err(StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={variant} target_ms={}",
                    position.as_millis()
                ))
                .into(),
            ));
        };
        let byte_offset = self.segment_byte_offset(seg_idx_u32).ok_or_else(|| {
            StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek offset not found: variant={variant} segment={seg_idx_u32}"
                ))
                .into(),
            )
        })?;
        let fetch_start = self.seek_readahead_start_segment(seg_idx_u32);
        let prefetch_anchor = self.segment_byte_offset(fetch_start).ok_or_else(|| {
            StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek prefetch offset not found: variant={variant} segment={fetch_start}"
                ))
                .into(),
            )
        })?;
        let anchor = SourceSeekAnchor::builder()
            .byte_offset(byte_offset)
            .segment_start(segment_start)
            .segment_end(segment_end)
            .segment_index(seg_idx_u32)
            .variant_index(variant)
            .build();
        self.set_prefetch_anchor(prefetch_anchor);
        self.set_seek_alias(byte_offset, seg_idx_u32);
        self.set_segment_aware_seek_tail(fetch_start);
        self.set_exact_seek_demand(byte_offset, seg_idx_u32);
        Ok(Some(anchor))
    }

    /// Reset variant to a "fresh" single-variant layout: `byte_shift = 0`,
    /// `served_from = 0`, `served_until = num_segments`. Called from
    /// [`HlsCoord::reset_for_seek`] so a random seek collapses the
    /// cross-variant byte continuity layering — variants archived from
    /// earlier auto-switches no longer co-serve the byte address space,
    /// and the (single) active variant addresses its segments by their
    /// natural offsets. Subsequent ABR commits at boundary will re-build
    /// the layering as usual.
    pub(crate) fn reset_to_full_range(&self) {
        self.clear_seek_alias();
        self.clear_exact_seek();
        self.clear_exact_byte_seek();
        self.clear_segment_aware_seek_tail();
        self.reset_layout_to_full_range();
    }

    pub(super) fn resolve_seek_alias(&self, demand: ExactSeekDemand, exact_anchor: u64) {
        // Off-RT (exact-prefix settle): a lock-free, alloc-free store of the
        // resolved exact anchor onto the matching base, tagged with the base
        // generation so a stale resolver cannot attach to a newer alias.
        self.seek
            .alias
            .resolve(demand.segment, demand.anchor, exact_anchor);
    }

    pub(crate) fn resolved_seek_anchor(&self, anchor: u64) -> Option<u64> {
        self.seek
            .alias
            .load()
            .filter(|alias| alias.anchor == anchor)
            .and_then(|alias| alias.exact_anchor)
    }

    pub(crate) fn resolved_seek_projection(&self, anchor: u64) -> Option<ResolvedSeekProjection> {
        let alias = self
            .seek
            .alias
            .load()
            .filter(|alias| alias.anchor == anchor)?;
        let demand_generation = self
            .seek
            .exact_seek
            .load()
            .filter(|demand| demand.segment == alias.segment && demand.anchor == alias.anchor)
            .map(|demand| demand.generation);
        Some(ResolvedSeekProjection {
            demand_generation,
            alias_generation: alias.generation,
            exact_anchor: alias.exact_anchor?,
        })
    }

    pub(crate) fn retire_seek_projection(&self, projection: ResolvedSeekProjection) {
        if !self
            .seek
            .alias
            .clear_if_generation(projection.alias_generation)
        {
            return;
        }
        if let Some(generation) = projection.demand_generation {
            let _ = self.seek.exact_seek.clear_if_generation(generation);
        }
    }

    pub(crate) fn retire_seek_projection_if_moved(&self, pos: u64) {
        // RT-reachable (via `advance`): a lock-free, alloc-free load + atomic
        // clear. The base is single-writer (on-core), so the `Some -> None`
        // store never races a concurrent base writer.
        if self
            .seek
            .alias
            .load()
            .is_some_and(|alias| !alias.covers_position(pos))
        {
            self.seek.alias.clear();
            self.clear_exact_seek();
        }
    }

    pub(super) fn seek_alias_at(&self, byte: u64) -> Option<(u32, u64, u64)> {
        let alias = self.seek.alias.load()?;
        if byte < alias.anchor {
            return None;
        }
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            return self.segment_aware_seek_alias_at(alias, byte);
        }
        let size = self.segment_size(alias.segment as usize)?;
        let end = alias.anchor.saturating_add(size);
        (byte < end).then_some((alias.segment, alias.anchor, size))
    }

    pub(super) fn seek_readahead_start_segment(&self, target_segment: u32) -> u32 {
        if needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            target_segment
        } else {
            target_segment.saturating_sub(1)
        }
    }

    fn segment_aware_seek_alias_at(
        &self,
        alias: AliasSnapshot,
        byte: u64,
    ) -> Option<(u32, u64, u64)> {
        let mut offset = alias.anchor;
        for (idx, segment) in self
            .segments
            .iter()
            .enumerate()
            .skip(alias.segment as usize)
        {
            let size = segment.len();
            let end = offset.saturating_add(size);
            if byte < end {
                let idx = u32::try_from(idx).ok()?;
                return Some((idx, offset, size));
            }
            offset = end;
        }
        None
    }

    pub(super) fn segment_aware_seek_tail_complete(&self) -> bool {
        if needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            return false;
        }
        let start = self.seek.segment_aware_tail.load(Ordering::Acquire);
        if start == Self::NO_SEEK_TAIL {
            return false;
        }
        let start = start as usize;
        let Some(tail) = self.segments.get(start..) else {
            return false;
        };
        !tail.is_empty() && tail.iter().all(|segment| segment.size().is_exact())
    }

    pub(super) fn set_segment_aware_seek_tail(&self, segment: u32) {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            self.seek
                .segment_aware_tail
                .store(segment, Ordering::Release);
        }
    }

    delegate::delegate! {
        to self.seek.alias {
            #[call(clear)]
            pub(super) fn clear_seek_alias(&self);
            #[call(set)]
            pub(super) fn set_seek_alias(&self, anchor: u64, segment: u32);
        }
        to self {
            /// Seek reset is layout-only. Active body fetches stay live: segment-aware
            /// decoders re-resolve media ranges by segment index, and canceling the
            /// in-flight target segment would put a streaming seek behind tail prefetch.
            #[call(reset_layout_to_full_range)]
            pub(crate) fn reset_for_seek(&self);
            #[expr($.and_then(|idx| u32::try_from(idx).ok()))]
            #[call(index_at_time)]
            pub(crate) fn segment_index_at_time(&self, t: Duration) -> Option<u32>;
        }
    }
}
