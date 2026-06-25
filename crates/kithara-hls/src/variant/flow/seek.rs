use std::sync::atomic::Ordering;

use kithara_platform::time::Duration;
use kithara_stream::{SourceSeekAnchor, StreamError, StreamResult, needs_exact_byte_sizes};

use super::{HlsVariant, size::ExactSeekDemand};
use crate::{HlsError, playlist::PlaylistAccess};

#[derive(Clone, Copy)]
pub(super) struct SeekAlias {
    pub(super) segment: u32,
    pub(super) anchor: u64,
    exact_anchor: Option<u64>,
}

impl SeekAlias {
    fn covers_position(self, pos: u64) -> bool {
        pos == self.anchor || self.exact_anchor == Some(pos)
    }
}

impl HlsVariant {
    pub(super) fn clear_seek_alias(&self) {
        *self.seek.alias.lock() = None;
    }

    pub(super) fn clear_seek_alias_if_moved(&self, pos: u64) {
        let mut alias = self.seek.alias.lock();
        if alias.is_some_and(|entry| !entry.covers_position(pos)) {
            *alias = None;
        }
    }

    pub(super) fn seek_alias_at(&self, byte: u64) -> Option<(u32, u64, u64)> {
        let alias = *self.seek.alias.lock();
        let alias = alias?;
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

    fn segment_aware_seek_alias_at(&self, alias: SeekAlias, byte: u64) -> Option<(u32, u64, u64)> {
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

    pub(super) fn set_seek_alias(&self, anchor: u64, segment: u32) {
        *self.seek.alias.lock() = Some(SeekAlias {
            segment,
            anchor,
            exact_anchor: None,
        });
    }

    pub(super) fn resolve_seek_alias(&self, demand: ExactSeekDemand, exact_anchor: u64) {
        let mut alias = self.seek.alias.lock();
        if let Some(entry) = alias.as_mut()
            && entry.anchor == demand.anchor
            && entry.segment == demand.segment
        {
            entry.exact_anchor = Some(exact_anchor);
        }
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
        self.rearm_cancel();
    }

    /// Seek reset is layout-only. Active body fetches stay live: segment-aware
    /// decoders re-resolve media ranges by segment index, and canceling the
    /// in-flight target segment would put a streaming seek behind tail prefetch.
    pub(crate) fn reset_for_seek(&self) {
        self.reset_layout_to_full_range();
    }

    pub(crate) fn seek_time_anchor(
        &self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>> {
        let variant = self.variant;
        let Some((segment_index, segment_start, segment_end)) = self
            .playlist_state
            .find_seek_point_for_time(variant, position)
        else {
            return Err(StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={variant} target_ms={}",
                    position.as_millis()
                ))
                .into(),
            ));
        };
        let seg_idx_u32 = u32::try_from(segment_index).unwrap_or(u32::MAX);
        let byte_offset = self.segment_byte_offset(seg_idx_u32).ok_or_else(|| {
            StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek offset not found: variant={variant} segment={segment_index}"
                ))
                .into(),
            )
        })?;
        let fetch_start = self.seek_readahead_start_segment(seg_idx_u32);
        let prefetch_anchor = self.segment_byte_offset(fetch_start).unwrap_or(byte_offset);
        let anchor = SourceSeekAnchor::builder()
            .byte_offset(byte_offset)
            .segment_start(segment_start)
            .segment_end(segment_end)
            .segment_index(seg_idx_u32)
            .variant_index(variant)
            .build();
        self.set_position_without_byte_demand(byte_offset);
        self.set_prefetch_anchor(prefetch_anchor);
        self.set_seek_alias(byte_offset, seg_idx_u32);
        self.set_segment_aware_seek_tail(fetch_start);
        self.rebuild_queue(fetch_start);
        self.set_exact_seek_demand(byte_offset, seg_idx_u32);
        Ok(Some(anchor))
    }

    pub(crate) fn segment_index_at_time(&self, t: Duration) -> Option<u32> {
        self.index_at_time(t)
            .and_then(|idx| u32::try_from(idx).ok())
    }

    fn clear_segment_aware_seek_tail(&self) {
        self.seek
            .segment_aware_tail
            .store(Self::NO_SEEK_TAIL, Ordering::Release);
    }

    pub(super) fn set_segment_aware_seek_tail(&self, segment: u32) {
        if !needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            self.seek
                .segment_aware_tail
                .store(segment, Ordering::Release);
        }
    }

    pub(super) fn seek_readahead_start_segment(&self, target_segment: u32) -> u32 {
        if needs_exact_byte_sizes(self.profile.codec, self.profile.container) {
            target_segment
        } else {
            target_segment.saturating_sub(1)
        }
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
}
