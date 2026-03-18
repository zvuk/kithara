//! Unified segment index with per-variant storage and stream composition.
//!
//! `StreamIndex` replaces `DownloadState` with a cleaner architecture:
//! - Per-variant `VariantSegments` stores actual (post-download) segment data
//! - `variant_map` (`RangeMap<usize, usize>`) assigns segment ranges to variants
//! - `byte_map` (`RangeMap<u64, (usize, usize)>`) provides O(log n) byte offset lookups
//! - `total_bytes` is derived (committed actual + estimated remaining), never formula-computed

use std::{collections::BTreeMap, ops::Range};

use kithara_assets::ResourceKey;
use kithara_stream::LayoutIndex;
use rangemap::RangeMap;
use tracing::trace;
use url::Url;

use crate::{
    ids::{SegmentIndex, VariantIndex},
    playlist::PlaylistAccess,
};

// SegmentData

/// Actual data for a committed segment (post-download, post-DRM-decrypt).
///
/// Unlike `LoadedSegment`, this does NOT store `variant` or `byte_offset` —
/// variant is known from the per-variant storage, `byte_offset` is computed
/// by the compositor from the stream layout.
#[derive(Debug, Clone)]
pub struct SegmentData {
    /// Size of the init segment in bytes (0 if no init).
    pub init_len: u64,
    /// Size of the media segment in bytes.
    pub media_len: u64,
    /// Absolute URL of the init segment (fMP4 only).
    pub init_url: Option<Url>,
    /// Absolute URL of the media segment.
    pub media_url: Url,
}

impl SegmentData {
    /// Total size of this segment (init + media).
    #[must_use]
    pub fn total_len(&self) -> u64 {
        self.init_len + self.media_len
    }
}

#[derive(Debug, Clone)]
struct SegmentSlot {
    available: bool,
    data: SegmentData,
}

// VariantSegments

/// All committed segments for one HLS variant.
///
/// Keyed by segment index (natural HLS key), not byte offset.
/// Byte offsets are computed by `StreamIndex` from the stream layout.
#[derive(Debug, Default)]
pub struct VariantSegments {
    segments: BTreeMap<SegmentIndex, SegmentSlot>,
}

impl VariantSegments {
    /// Create empty variant storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a committed segment.
    pub fn insert(&mut self, segment_index: SegmentIndex, data: SegmentData) {
        self.segments.insert(
            segment_index,
            SegmentSlot {
                available: true,
                data,
            },
        );
    }

    /// Get segment data by index.
    #[must_use]
    pub fn get(&self, segment_index: SegmentIndex) -> Option<&SegmentData> {
        self.segments
            .get(&segment_index)
            .filter(|slot| slot.available)
            .map(|slot| &slot.data)
    }

    #[must_use]
    fn get_stored(&self, segment_index: SegmentIndex) -> Option<&SegmentData> {
        self.segments.get(&segment_index).map(|slot| &slot.data)
    }

    /// Mark a segment unavailable while preserving its last known lengths.
    pub fn invalidate(&mut self, segment_index: SegmentIndex) -> bool {
        let Some(slot) = self.segments.get_mut(&segment_index) else {
            return false;
        };
        let was_available = slot.available;
        slot.available = false;
        was_available
    }

    /// Whether a segment is committed.
    #[must_use]
    pub fn contains(&self, segment_index: SegmentIndex) -> bool {
        self.segments
            .get(&segment_index)
            .is_some_and(|slot| slot.available)
    }

    /// Remove all segments.
    pub fn clear(&mut self) {
        self.segments.clear();
    }

    /// First committed segment (lowest index).
    #[must_use]
    pub fn first(&self) -> Option<(SegmentIndex, &SegmentData)> {
        self.iter().next()
    }

    /// Last committed segment (highest index).
    #[must_use]
    pub fn last(&self) -> Option<(SegmentIndex, &SegmentData)> {
        self.iter().next_back()
    }

    /// Iterate committed segments in order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (SegmentIndex, &SegmentData)> {
        self.segments
            .iter()
            .filter(|(_, slot)| slot.available)
            .map(|(&k, slot)| (k, &slot.data))
    }

    fn iter_all(&self) -> impl Iterator<Item = (SegmentIndex, &SegmentSlot)> {
        self.segments.iter().map(|(&k, slot)| (k, slot))
    }

    /// Number of committed segments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether there are no committed segments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

// SegmentRef

/// Reference to a committed segment with its computed byte offset.
///
/// Returned by `StreamIndex::find_at_offset`.
#[derive(Debug)]
pub struct SegmentRef<'a> {
    /// Variant index in the master playlist.
    pub variant: VariantIndex,
    /// Segment index within the variant's media playlist.
    pub segment_index: SegmentIndex,
    /// Computed byte offset in the virtual stream.
    pub byte_offset: u64,
    /// Actual segment data (sizes, URLs).
    pub data: &'a SegmentData,
}

// StreamIndex

/// Single source of truth for HLS segment layout.
///
/// Combines per-variant segment storage with stream-level composition.
/// `total_bytes` is derived from committed actual sizes + estimated remaining
/// from `VariantSizeMap` — no formulas, no delta patching.
pub struct StreamIndex {
    /// Absolute byte offset of the first visible logical segment.
    layout_base_offset: u64,
    /// Per-variant committed segment data, indexed by variant number.
    variants: Vec<VariantSegments>,
    /// Segment-index ranges → variant. Covers all segments (committed + expected).
    /// ABR switch appends a new region; reset replaces all.
    variant_map: RangeMap<SegmentIndex, VariantIndex>,
    /// Byte-offset ranges → (variant, `segment_index`). Only committed segments.
    /// Updated on every commit/reconcile. Provides O(log n) `find_at_offset`.
    byte_map: RangeMap<u64, (VariantIndex, SegmentIndex)>,
    /// Expected total size per segment per variant (from `size_map` HEAD estimates).
    /// Used by `rebuild_byte_map_from` to reserve space for uncommitted segments.
    expected_sizes: Vec<Vec<u64>>,
    /// Total number of segments in the playlist.
    num_segments: usize,
}

impl StreamIndex {
    /// Create a new stream index.
    ///
    /// `num_variants` — number of variants in the master playlist.
    /// `num_segments` — total number of segments per variant.
    /// Default layout: variant 0 owns all segments.
    #[must_use]
    pub fn new(num_variants: usize, num_segments: usize) -> Self {
        let mut variant_map = RangeMap::new();
        if num_segments > 0 {
            variant_map.insert(0..num_segments, 0);
        }
        Self {
            layout_base_offset: 0,
            variants: (0..num_variants).map(|_| VariantSegments::new()).collect(),
            variant_map,
            byte_map: RangeMap::new(),
            expected_sizes: vec![Vec::new(); num_variants],
            num_segments,
        }
    }

    // --- Variant map queries ---

    /// Which variant is assigned to this segment index?
    #[must_use]
    pub fn variant_at(&self, segment_index: SegmentIndex) -> Option<VariantIndex> {
        self.variant_map.get(&segment_index).copied()
    }

    /// Total number of segments in the playlist.
    #[must_use]
    pub fn num_segments(&self) -> usize {
        self.num_segments
    }

    // --- Mutations: variant layout ---

    /// ABR switch: assign new variant from `segment_index` to end.
    ///
    /// `RangeMap::insert` auto-splits/replaces overlapping regions.
    pub fn switch_variant(&mut self, segment_index: SegmentIndex, variant: VariantIndex) {
        if segment_index < self.num_segments {
            trace!(segment_index, variant, "stream_index::switch_variant");
            self.variant_map
                .insert(segment_index..self.num_segments, variant);
        }
    }

    /// Reset seek: replace entire layout with single region.
    ///
    /// Variant data in `VariantSegments` is preserved (can be reused on switch-back).
    pub fn reset_to(
        &mut self,
        segment_index: SegmentIndex,
        variant: VariantIndex,
        base_offset: u64,
    ) {
        trace!(
            segment_index,
            variant, base_offset, "stream_index::reset_to"
        );
        self.layout_base_offset = if segment_index < self.num_segments {
            base_offset
        } else {
            0
        };
        self.variant_map.clear();
        if segment_index < self.num_segments {
            self.variant_map
                .insert(segment_index..self.num_segments, variant);
        }
        // byte_map is NOT cleared — committed segments from preserved
        // variants may still be valid. rebuild_byte_map_from(0) will
        // recompute which entries are visible under the new layout.
        self.rebuild_byte_map_from(0);
    }

    /// Set expected total sizes per segment (from `size_map` HEAD estimates).
    ///
    /// Called once after `calculate_size_map`. Enables `rebuild_byte_map_from`
    /// to reserve correct offsets for not-yet-committed segments.
    pub fn set_expected_sizes(&mut self, variant: VariantIndex, sizes: Vec<u64>) {
        trace!(variant, count = sizes.len(), "set_expected_sizes");
        if variant < self.expected_sizes.len() {
            self.expected_sizes[variant] = sizes;
        }
        self.rebuild_byte_map_from(0);
    }

    // --- Mutations: segment data ---

    /// Commit a downloaded segment.
    ///
    /// Computes byte offset from preceding committed segments, stores actual
    /// data in per-variant storage, and updates `byte_map`.
    pub fn commit_segment(
        &mut self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        data: SegmentData,
    ) {
        trace!(
            variant,
            segment_index,
            total_len = data.total_len(),
            "stream_index::commit_segment"
        );
        self.variants[variant].insert(segment_index, data);
        self.rebuild_byte_map_from(segment_index);
    }

    /// DRM reconciliation: segment actual size differs from HEAD estimate.
    ///
    /// Replaces segment data and rebuilds `byte_map` from that segment onward
    /// (subsequent committed segments shift to maintain contiguity).
    pub fn reconcile_segment(
        &mut self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        new_data: SegmentData,
    ) {
        trace!(
            variant,
            segment_index,
            new_total_len = new_data.total_len(),
            "stream_index::reconcile_segment"
        );
        self.variants[variant].insert(segment_index, new_data);
        self.rebuild_byte_map_from(segment_index);
    }

    /// Cache invalidation: remove a single segment.
    ///
    /// Called by `on_invalidated` callback when `CachedAssets` displaces a resource.
    pub fn on_segment_invalidated(&mut self, variant: VariantIndex, segment_index: SegmentIndex) {
        if let Some(vs) = self.variants.get_mut(variant)
            && vs.contains(segment_index)
        {
            trace!(
                variant,
                segment_index, "stream_index::on_segment_invalidated"
            );
            vs.invalidate(segment_index);
            self.rebuild_byte_map_from(0);
        }
    }

    // --- Reads ---

    /// Find the committed segment containing the given byte offset.
    ///
    /// O(log n) via `byte_map`. Returns `None` if offset is in a gap
    /// (uncommitted segment) or past all committed data.
    #[must_use]
    pub fn find_at_offset(&self, offset: u64) -> Option<SegmentRef<'_>> {
        let (range, &(variant, seg_idx)) = self.byte_map.get_key_value(&offset)?;
        let data = self.variants[variant].get(seg_idx)?;
        Some(SegmentRef {
            variant,
            segment_index: seg_idx,
            byte_offset: range.start,
            data,
        })
    }

    /// Find the visible segment containing the given byte offset.
    #[must_use]
    pub fn visible_segment_at(&self, offset: u64) -> Option<SegmentRef<'_>> {
        self.find_at_offset(offset)
    }

    /// Byte range of a committed segment in the current composed layout.
    #[must_use]
    pub fn range_for(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<Range<u64>> {
        self.item_range((variant, segment_index))
    }

    #[must_use]
    pub(crate) fn layout_floor_segment(&self) -> Option<(VariantIndex, SegmentIndex)> {
        self.variant_map
            .iter()
            .next()
            .map(|(seg_range, &variant)| (variant, seg_range.start))
    }

    /// Byte offset of a logical segment in the current applied layout.
    #[must_use]
    pub(crate) fn layout_offset_for_segment(
        &self,
        target_segment: SegmentIndex,
        playlist: &dyn PlaylistAccess,
    ) -> Option<u64> {
        let mut offset = self.layout_base_offset;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                if seg_idx == target_segment {
                    return Some(offset);
                }
                let seg_len = self
                    .stored_segment(variant, seg_idx)
                    .map(SegmentData::total_len)
                    .or_else(|| playlist.segment_size(variant, seg_idx))?;
                offset = offset.saturating_add(seg_len);
            }
        }
        None
    }

    /// Whether a committed segment is visible in the current composed layout.
    #[must_use]
    pub fn is_visible(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.range_for(variant, segment_index).is_some()
    }

    /// Highest committed byte offset (watermark).
    #[must_use]
    pub fn max_end_offset(&self) -> u64 {
        self.byte_map
            .iter()
            .next_back()
            .map_or(0, |(range, _)| range.end)
    }

    /// Whether a segment has been committed.
    #[must_use]
    pub fn is_segment_loaded(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.variants
            .get(variant)
            .is_some_and(|vs| vs.contains(segment_index))
    }

    /// Whether the entire byte range is covered by committed segments.
    #[must_use]
    pub fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        self.byte_map.gaps(range).next().is_none()
    }

    /// Derived total bytes: committed actual + estimated remaining.
    ///
    /// For each segment in the `variant_map`:
    /// - Committed → actual size (from `VariantSegments`)
    /// - Not committed → estimated size (from `PlaylistState` / `VariantSizeMap`)
    ///
    /// Monotonically converges to the true value as segments download.
    /// DRM streams slightly overestimate (encrypted > decrypted) — safe.
    #[must_use]
    pub(crate) fn total_bytes(&self, playlist: &dyn PlaylistAccess) -> u64 {
        let mut total = self.layout_base_offset;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                total += self.stored_segment(variant, seg_idx).map_or_else(
                    || playlist.segment_size(variant, seg_idx).unwrap_or(0),
                    SegmentData::total_len,
                );
            }
        }
        total
    }

    /// Effective total: max of committed watermark and estimated total.
    #[must_use]
    pub(crate) fn effective_total(&self, playlist: &dyn PlaylistAccess) -> u64 {
        self.max_end_offset().max(self.total_bytes(playlist))
    }

    #[must_use]
    pub(crate) fn segment_for_offset(
        &self,
        offset: u64,
        playlist: &dyn PlaylistAccess,
    ) -> Option<(VariantIndex, SegmentIndex)> {
        if let Some(seg_ref) = self.find_at_offset(offset) {
            return Some((seg_ref.variant, seg_ref.segment_index));
        }

        let mut cursor = self.layout_base_offset;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                let seg_len = self
                    .stored_segment(variant, seg_idx)
                    .map(SegmentData::total_len)
                    .or_else(|| playlist.segment_size(variant, seg_idx))?;

                let end = cursor.saturating_add(seg_len);
                if offset < end {
                    return Some((variant, seg_idx));
                }
                cursor = end;
            }
        }

        None
    }

    /// Number of expected sizes stored (across all variants).
    #[must_use]
    pub fn expected_sizes_len(&self) -> usize {
        self.expected_sizes.iter().map(Vec::len).sum()
    }

    /// Number of committed segments across all variants.
    #[must_use]
    pub fn num_committed(&self) -> usize {
        self.byte_map.iter().count()
    }

    /// Access per-variant storage.
    #[must_use]
    pub fn variant_segments(&self, variant: VariantIndex) -> Option<&VariantSegments> {
        self.variants.get(variant)
    }

    #[must_use]
    pub(crate) fn stored_segment(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<&SegmentData> {
        self.variants.get(variant)?.get_stored(segment_index)
    }

    /// Remove every committed segment that depends on the invalidated resource.
    ///
    /// A shared init resource can invalidate multiple visible segments.
    /// Returns true if any committed coverage was removed.
    pub fn remove_resource(&mut self, key: &ResourceKey) -> bool {
        let mut removed = false;
        let mut first_removed: Option<SegmentIndex> = None;

        for vs in &mut self.variants {
            let to_remove: Vec<usize> = vs
                .iter_all()
                .filter_map(|(segment_index, slot)| {
                    let init_matches = slot
                        .data
                        .init_url
                        .as_ref()
                        .is_some_and(|url| ResourceKey::from_url(url) == *key);
                    let media_matches = ResourceKey::from_url(&slot.data.media_url) == *key;
                    (init_matches || media_matches).then_some(segment_index)
                })
                .collect();

            if to_remove.is_empty() {
                continue;
            }

            removed = true;
            for segment_index in to_remove {
                first_removed =
                    Some(first_removed.map_or(segment_index, |current| current.min(segment_index)));
                vs.invalidate(segment_index);
            }
        }

        if let Some(from_segment) = first_removed {
            self.rebuild_byte_map_from(from_segment);
        }

        removed
    }

    // --- Internal ---

    /// Compute the byte offset where `target_segment` starts in the virtual stream.
    ///
    /// Walks segments in layout order, summing their sizes.
    /// Uses committed size when available, falling back to expected size.
    fn byte_offset_at(&self, target_segment: SegmentIndex) -> u64 {
        let mut offset = self.layout_base_offset;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                if seg_idx >= target_segment {
                    return offset;
                }
                let seg_len = self
                    .stored_segment(variant, seg_idx)
                    .map(SegmentData::total_len)
                    .or_else(|| {
                        self.expected_sizes
                            .get(variant)
                            .and_then(|v| v.get(seg_idx).copied())
                    })
                    .unwrap_or(0);
                offset += seg_len;
            }
        }
        offset
    }

    /// Rebuild `byte_map` entries for all committed segments from `from_segment` onward.
    ///
    /// This is the replacement for `cascade_contiguity` — when a segment's size
    /// changes (DRM reconciliation) or a segment is added/removed, all subsequent
    /// byte offsets shift to maintain contiguity.
    fn rebuild_byte_map_from(&mut self, from_segment: SegmentIndex) {
        // Remove old entries for segments >= from_segment
        let keys_to_remove: Vec<Range<u64>> = self
            .byte_map
            .iter()
            .filter(|&(_, &(_, seg_idx))| seg_idx >= from_segment)
            .map(|(range, _)| range.clone())
            .collect();
        for range in keys_to_remove {
            self.byte_map.remove(range);
        }

        // Reinsert with correct offsets.
        // For uncommitted segments, advance offset by expected size so that
        // subsequent committed segments appear at their canonical byte offset.
        let mut offset = self.byte_offset_at(from_segment);
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                if seg_idx < from_segment {
                    continue;
                }
                let stored_len = self
                    .stored_segment(variant, seg_idx)
                    .map(SegmentData::total_len);
                let total_len = stored_len
                    .or_else(|| {
                        self.expected_sizes
                            .get(variant)
                            .and_then(|v| v.get(seg_idx).copied())
                    })
                    .unwrap_or(0);
                if total_len == 0 {
                    continue;
                }
                if stored_len.is_some() && self.is_segment_loaded(variant, seg_idx) {
                    let end = offset + total_len;
                    self.byte_map.insert(offset..end, (variant, seg_idx));
                }
                offset += total_len;
            }
        }
    }
}

// LayoutIndex

impl LayoutIndex for StreamIndex {
    type Item = (VariantIndex, SegmentIndex);

    fn item_at_offset(&self, offset: u64) -> Option<Self::Item> {
        self.byte_map.get(&offset).copied()
    }

    fn item_range(&self, (variant, seg_idx): Self::Item) -> Option<Range<u64>> {
        self.byte_map
            .iter()
            .find(|&(_, &(v, s))| v == variant && s == seg_idx)
            .map(|(range, _)| range.clone())
    }
}

#[cfg(test)]
mod tests {
    use kithara_assets::ResourceKey;
    use kithara_test_utils::kithara;
    use url::Url;

    use super::*;

    fn base_url() -> Url {
        Url::parse("https://cdn.example.com/audio/").expect("valid url")
    }

    fn make_segment_data(init_len: u64, media_len: u64) -> SegmentData {
        SegmentData {
            init_len,
            media_len,
            init_url: if init_len > 0 {
                Some(base_url().join("init.mp4").expect("valid url"))
            } else {
                None
            },
            media_url: base_url().join("segment.m4s").expect("valid url"),
        }
    }

    // === SegmentData ===

    #[kithara::test]
    fn segment_data_total_len() {
        let data = make_segment_data(623, 50000);
        assert_eq!(data.total_len(), 50623);
    }

    #[kithara::test]
    fn segment_data_zero_init() {
        let data = make_segment_data(0, 48000);
        assert_eq!(data.total_len(), 48000);
        assert!(data.init_url.is_none());
    }

    // === VariantSegments ===

    #[kithara::test]
    fn variant_segments_insert_and_get() {
        let mut vs = VariantSegments::new();
        vs.insert(3, make_segment_data(100, 500));
        assert!(vs.contains(3));
        assert!(!vs.contains(4));
        assert_eq!(vs.get(3).expect("exists").total_len(), 600);
    }

    #[kithara::test]
    fn variant_segments_remove() {
        let mut vs = VariantSegments::new();
        vs.insert(0, make_segment_data(100, 500));
        vs.insert(1, make_segment_data(0, 400));
        assert!(vs.invalidate(0));
        assert!(!vs.contains(0));
        assert!(vs.contains(1));
    }

    #[kithara::test]
    fn variant_segments_clear() {
        let mut vs = VariantSegments::new();
        vs.insert(0, make_segment_data(100, 500));
        vs.insert(1, make_segment_data(0, 400));
        vs.clear();
        assert!(!vs.contains(0));
        assert!(!vs.contains(1));
        assert!(vs.is_empty());
    }

    #[kithara::test]
    fn variant_segments_first_last() {
        let mut vs = VariantSegments::new();
        vs.insert(2, make_segment_data(0, 200));
        vs.insert(5, make_segment_data(0, 500));
        vs.insert(0, make_segment_data(100, 100));

        let (idx, data) = vs.first().expect("has first");
        assert_eq!(idx, 0);
        assert_eq!(data.total_len(), 200);

        let (idx, data) = vs.last().expect("has last");
        assert_eq!(idx, 5);
        assert_eq!(data.total_len(), 500);
    }

    // === StreamIndex: variant_map ===

    #[kithara::test]
    fn stream_index_new_single_variant() {
        let idx = StreamIndex::new(4, 37);
        assert_eq!(idx.variant_at(0), Some(0));
        assert_eq!(idx.variant_at(36), Some(0));
        assert_eq!(idx.variant_at(37), None);
    }

    #[kithara::test]
    fn switch_variant_appends_region() {
        let mut idx = StreamIndex::new(4, 37);
        idx.switch_variant(3, 3);
        assert_eq!(idx.variant_at(0), Some(0));
        assert_eq!(idx.variant_at(2), Some(0));
        assert_eq!(idx.variant_at(3), Some(3));
        assert_eq!(idx.variant_at(36), Some(3));
    }

    #[kithara::test]
    fn switch_variant_truncates_tail() {
        let mut idx = StreamIndex::new(4, 37);
        idx.switch_variant(3, 3);
        idx.switch_variant(6, 0);
        assert_eq!(idx.variant_at(2), Some(0));
        assert_eq!(idx.variant_at(4), Some(3));
        assert_eq!(idx.variant_at(6), Some(0));
        assert_eq!(idx.variant_at(36), Some(0));
    }

    #[kithara::test]
    fn reset_to_replaces_layout() {
        let mut idx = StreamIndex::new(4, 37);
        idx.switch_variant(3, 3);
        idx.reset_to(5, 2, 0);
        assert_eq!(idx.variant_at(4), None);
        assert_eq!(idx.variant_at(5), Some(2));
        assert_eq!(idx.variant_at(36), Some(2));
    }

    // === StreamIndex: commit + byte_map ===

    #[kithara::test]
    fn commit_single_variant_builds_byte_map() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(623, 50000));
        idx.commit_segment(0, 1, make_segment_data(0, 48000));
        idx.commit_segment(0, 2, make_segment_data(0, 52000));

        assert_eq!(idx.max_end_offset(), 623 + 50000 + 48000 + 52000);
        assert!(idx.is_segment_loaded(0, 0));
        assert!(idx.is_segment_loaded(0, 2));
        assert!(!idx.is_segment_loaded(0, 3));
    }

    #[kithara::test]
    fn commit_with_abr_switch() {
        let mut idx = StreamIndex::new(4, 6);
        idx.commit_segment(0, 0, make_segment_data(623, 50000));
        idx.commit_segment(0, 1, make_segment_data(0, 50000));
        idx.commit_segment(0, 2, make_segment_data(0, 50000));
        idx.switch_variant(3, 3);
        idx.commit_segment(3, 3, make_segment_data(624, 725000));
        idx.commit_segment(3, 4, make_segment_data(0, 723000));
        idx.commit_segment(3, 5, make_segment_data(0, 724000));

        let v0_total: u64 = 623 + 50000 + 50000 + 50000;
        let v3_total: u64 = 624 + 725000 + 723000 + 724000;
        assert_eq!(idx.max_end_offset(), v0_total + v3_total);
    }

    #[kithara::test]
    fn reconcile_segment_shifts_subsequent_offsets() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(100, 500)); // 600
        idx.commit_segment(0, 1, make_segment_data(0, 400)); // 400
        idx.commit_segment(0, 2, make_segment_data(0, 300)); // 300
        assert_eq!(idx.max_end_offset(), 1300);

        // DRM reconciliation: segment 0 actual is smaller
        idx.reconcile_segment(0, 0, make_segment_data(90, 500));
        assert_eq!(idx.max_end_offset(), 1290);

        let seg1 = idx.find_at_offset(590).expect("seg 1 starts at 590");
        assert_eq!(seg1.segment_index, 1);
        assert_eq!(seg1.byte_offset, 590);
    }

    // === StreamIndex: find_at_offset ===

    #[kithara::test]
    fn find_at_offset_returns_correct_segment() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(100, 500)); // 0..600
        idx.commit_segment(0, 1, make_segment_data(0, 400)); // 600..1000
        idx.commit_segment(0, 2, make_segment_data(0, 300)); // 1000..1300

        let seg = idx.find_at_offset(0).expect("at 0");
        assert_eq!(seg.segment_index, 0);
        assert_eq!(seg.byte_offset, 0);

        let seg = idx.find_at_offset(599).expect("at 599");
        assert_eq!(seg.segment_index, 0);

        let seg = idx.find_at_offset(600).expect("at 600");
        assert_eq!(seg.segment_index, 1);
        assert_eq!(seg.byte_offset, 600);

        let seg = idx.find_at_offset(1299).expect("at 1299");
        assert_eq!(seg.segment_index, 2);

        assert!(idx.find_at_offset(1300).is_none());
    }

    #[kithara::test]
    fn find_at_offset_cross_variant() {
        let mut idx = StreamIndex::new(4, 6);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));
        idx.switch_variant(2, 3);
        idx.commit_segment(3, 2, make_segment_data(50, 700));

        let seg = idx.find_at_offset(250).expect("in v3");
        assert_eq!(seg.variant, 3);
        assert_eq!(seg.segment_index, 2);
        assert_eq!(seg.byte_offset, 200);
    }

    // === StreamIndex: is_range_loaded ===

    #[kithara::test]
    fn is_range_loaded_contiguous() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));

        assert!(idx.is_range_loaded(&(0..200)));
        assert!(idx.is_range_loaded(&(50..150)));
        assert!(!idx.is_range_loaded(&(0..300)));
    }

    // === StreamIndex: on_segment_invalidated ===

    #[kithara::test]
    fn on_segment_invalidated_removes_from_byte_map() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));
        idx.commit_segment(0, 2, make_segment_data(0, 100));

        idx.on_segment_invalidated(0, 1);
        assert!(!idx.is_segment_loaded(0, 1));
        assert!(idx.is_segment_loaded(0, 0));
        assert!(idx.is_segment_loaded(0, 2));
        assert!(
            idx.find_at_offset(150).is_none(),
            "invalidated segment must leave a hole in the byte layout"
        );
        let seg = idx
            .find_at_offset(250)
            .expect("segment 2 must keep its original offset");
        assert_eq!(seg.segment_index, 2);
        assert_eq!(seg.byte_offset, 200);
        assert_eq!(idx.max_end_offset(), 300);
    }

    #[kithara::test]
    fn on_segment_invalidated_unknown_is_noop() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.on_segment_invalidated(0, 5); // unknown
        assert_eq!(idx.max_end_offset(), 100);
    }

    #[kithara::test]
    fn remove_resource_invalidates_visible_segment() {
        let mut idx = StreamIndex::new(1, 2);
        let media_url = base_url().join("segment-0.m4s").expect("valid url");
        idx.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );

        let removed = idx.remove_resource(&ResourceKey::from_url(&media_url));

        assert!(
            removed,
            "resource invalidation must report that it removed coverage"
        );
        assert!(
            idx.find_at_offset(0).is_none(),
            "invalidated resource must disappear from visible byte layout"
        );
    }

    #[kithara::test]
    fn remove_resource_preserves_following_segment_offsets() {
        let mut idx = StreamIndex::new(1, 3);
        let media_url_0 = base_url().join("segment-0.m4s").expect("valid url");
        let media_url_1 = base_url().join("segment-1.m4s").expect("valid url");
        let media_url_2 = base_url().join("segment-2.m4s").expect("valid url");

        idx.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url_0.clone(),
            },
        );
        idx.commit_segment(
            0,
            1,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url_1.clone(),
            },
        );
        idx.commit_segment(
            0,
            2,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url_2,
            },
        );

        let removed = idx.remove_resource(&ResourceKey::from_url(&media_url_1));

        assert!(removed);
        assert!(
            idx.find_at_offset(150).is_none(),
            "the invalidated segment must leave a hole"
        );
        let seg = idx
            .find_at_offset(250)
            .expect("segment 2 must keep its original offset");
        assert_eq!(seg.segment_index, 2);
        assert_eq!(seg.byte_offset, 200);
    }

    // === StreamIndex: multiple switches consistency ===

    #[kithara::test]
    fn multiple_switches_total_bytes_consistent() {
        let mut idx = StreamIndex::new(4, 6);
        // v0 segments 0-1 (actual decrypted: 90 each)
        idx.commit_segment(0, 0, make_segment_data(0, 90));
        idx.commit_segment(0, 1, make_segment_data(0, 90));
        // Switch to v3 at segment 2
        idx.switch_variant(2, 3);
        idx.commit_segment(3, 2, make_segment_data(0, 985));
        idx.commit_segment(3, 3, make_segment_data(0, 985));
        // Switch back to v0 at segment 4
        idx.switch_variant(4, 0);
        idx.commit_segment(0, 4, make_segment_data(0, 90));

        // Verify byte layout: 90 + 90 + 985 + 985 + 90 = 2240
        assert_eq!(idx.max_end_offset(), 2240);

        // Verify find_at_offset across variants
        // Layout: seg0(v0:0..90) seg1(v0:90..180) seg2(v3:180..1165) seg3(v3:1165..2150) seg4(v0:2150..2240)
        let seg = idx.find_at_offset(50).expect("in v0 prefix");
        assert_eq!(seg.variant, 0);
        assert_eq!(seg.segment_index, 0);

        let seg = idx.find_at_offset(200).expect("in v3 region");
        assert_eq!(seg.variant, 3);
        assert_eq!(seg.segment_index, 2);

        let seg = idx.find_at_offset(2200).expect("in v0 suffix");
        assert_eq!(seg.variant, 0);
        assert_eq!(seg.segment_index, 4);
    }

    // === LayoutIndex ===

    #[kithara::test]
    fn layout_index_item_at_offset() {
        let mut idx = StreamIndex::new(2, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.switch_variant(1, 1);
        idx.commit_segment(1, 1, make_segment_data(0, 200));

        assert_eq!(idx.item_at_offset(50), Some((0, 0)));
        assert_eq!(idx.item_at_offset(150), Some((1, 1)));
        assert_eq!(idx.item_at_offset(300), None);
    }

    #[kithara::test]
    fn layout_index_item_range() {
        let mut idx = StreamIndex::new(1, 2);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 200));

        assert_eq!(idx.item_range((0, 0)), Some(0..100));
        assert_eq!(idx.item_range((0, 1)), Some(100..300));
        assert_eq!(idx.item_range((0, 5)), None);
    }

    // === StreamIndex: sparse commit (gap between committed segments) ===

    #[kithara::test]
    fn find_at_offset_with_gap_preserves_layout_offsets() {
        // 20 segments, each 200 bytes. Commit 0-3 then skip to 16.
        let mut idx = StreamIndex::new(1, 20);
        idx.set_expected_sizes(0, vec![200; 20]);
        for i in 0..4 {
            idx.commit_segment(0, i, make_segment_data(0, 200));
        }
        // Skip segments 4-15 (not downloaded yet).
        idx.commit_segment(0, 16, make_segment_data(0, 200));

        // Segment 16 should be at offset 16*200 = 3200, not 4*200 = 800.
        let seg = idx
            .find_at_offset(3200)
            .expect("segment 16 must be findable at its layout offset 3200");
        assert_eq!(seg.segment_index, 16);
        assert_eq!(seg.byte_offset, 3200);
    }

    #[kithara::test]
    fn reset_to_variant_without_expected_sizes_collapses_gaps() {
        // Proves the bug: reset_to variant 1 without expected_sizes[1]
        // causes segment 8 to collapse to base offset 500.
        let mut idx = StreamIndex::new(2, 10);
        idx.set_expected_sizes(0, vec![100; 10]);
        // variant 1 has NO expected_sizes (ensure_variant_ready not called)

        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.reset_to(5, 1, 500);
        idx.commit_segment(1, 8, make_segment_data(0, 120));

        // BUG: segment 8 at 500 (collapsed) instead of 500 + 3*? = ???
        assert!(
            idx.find_at_offset(500).is_some(),
            "without expected_sizes[1], segment 8 collapses to base"
        );
    }

    #[kithara::test]
    fn reset_to_variant_with_expected_sizes_preserves_offsets() {
        // After setting expected_sizes for variant 1, gaps are preserved.
        let mut idx = StreamIndex::new(2, 10);
        idx.set_expected_sizes(0, vec![100; 10]);
        idx.set_expected_sizes(1, vec![120; 10]);

        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.reset_to(5, 1, 500);
        idx.commit_segment(1, 8, make_segment_data(0, 120));

        // segment 8 = base(500) + 3 gaps * 120 = 500 + 360 = 860
        let seg = idx
            .find_at_offset(860)
            .expect("segment 8 with expected_sizes must be at 860");
        assert_eq!(seg.segment_index, 8);
        assert_eq!(seg.byte_offset, 860);
    }
}
