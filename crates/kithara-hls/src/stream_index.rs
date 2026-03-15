//! Unified segment index with per-variant storage and stream composition.
//!
//! `StreamIndex` replaces `DownloadState` with a cleaner architecture:
//! - Per-variant `VariantSegments` stores actual (post-download) segment data
//! - `variant_map` (`RangeMap<usize, usize>`) assigns segment ranges to variants
//! - `byte_map` (`RangeMap<u64, (usize, usize)>`) provides O(log n) byte offset lookups
//! - `total_bytes` is derived (committed actual + estimated remaining), never formula-computed

use std::{collections::BTreeMap, ops::Range};

use kithara_stream::LayoutIndex;
use rangemap::RangeMap;
use tracing::trace;
use url::Url;

use crate::playlist::PlaylistAccess;

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

// VariantSegments

/// All committed segments for one HLS variant.
///
/// Keyed by segment index (natural HLS key), not byte offset.
/// Byte offsets are computed by `StreamIndex` from the stream layout.
#[derive(Debug, Default)]
pub struct VariantSegments {
    segments: BTreeMap<usize, SegmentData>,
}

impl VariantSegments {
    /// Create empty variant storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a committed segment.
    pub fn insert(&mut self, segment_index: usize, data: SegmentData) {
        self.segments.insert(segment_index, data);
    }

    /// Get segment data by index.
    #[must_use]
    pub fn get(&self, segment_index: usize) -> Option<&SegmentData> {
        self.segments.get(&segment_index)
    }

    /// Remove a segment (e.g. after cache invalidation).
    pub fn remove(&mut self, segment_index: usize) {
        self.segments.remove(&segment_index);
    }

    /// Whether a segment is committed.
    #[must_use]
    pub fn contains(&self, segment_index: usize) -> bool {
        self.segments.contains_key(&segment_index)
    }

    /// Remove all segments.
    pub fn clear(&mut self) {
        self.segments.clear();
    }

    /// First committed segment (lowest index).
    #[must_use]
    pub fn first(&self) -> Option<(usize, &SegmentData)> {
        self.segments.iter().next().map(|(&k, v)| (k, v))
    }

    /// Last committed segment (highest index).
    #[must_use]
    pub fn last(&self) -> Option<(usize, &SegmentData)> {
        self.segments.iter().next_back().map(|(&k, v)| (k, v))
    }

    /// Iterate committed segments in order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &SegmentData)> {
        self.segments.iter().map(|(&k, v)| (k, v))
    }

    /// Number of committed segments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.segments.len()
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
    pub variant: usize,
    /// Segment index within the variant's media playlist.
    pub segment_index: usize,
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
    /// Per-variant committed segment data, indexed by variant number.
    variants: Vec<VariantSegments>,
    /// Segment-index ranges → variant. Covers all segments (committed + expected).
    /// ABR switch appends a new region; reset replaces all.
    variant_map: RangeMap<usize, usize>,
    /// Byte-offset ranges → (variant, `segment_index`). Only committed segments.
    /// Updated on every commit/reconcile. Provides O(log n) `find_at_offset`.
    byte_map: RangeMap<u64, (usize, usize)>,
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
            variants: (0..num_variants).map(|_| VariantSegments::new()).collect(),
            variant_map,
            byte_map: RangeMap::new(),
            num_segments,
        }
    }

    // --- Variant map queries ---

    /// Which variant is assigned to this segment index?
    #[must_use]
    pub fn variant_at(&self, segment_index: usize) -> Option<usize> {
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
    pub fn switch_variant(&mut self, segment_index: usize, variant: usize) {
        if segment_index < self.num_segments {
            trace!(segment_index, variant, "stream_index::switch_variant");
            self.variant_map
                .insert(segment_index..self.num_segments, variant);
        }
    }

    /// Reset seek: replace entire layout with single region.
    ///
    /// Variant data in `VariantSegments` is preserved (can be reused on switch-back).
    pub fn reset_to(&mut self, segment_index: usize, variant: usize) {
        trace!(segment_index, variant, "stream_index::reset_to");
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

    // --- Mutations: segment data ---

    /// Commit a downloaded segment.
    ///
    /// Computes byte offset from preceding committed segments, stores actual
    /// data in per-variant storage, and updates `byte_map`.
    pub fn commit_segment(&mut self, variant: usize, segment_index: usize, data: SegmentData) {
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
        variant: usize,
        segment_index: usize,
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
    pub fn on_segment_invalidated(&mut self, variant: usize, segment_index: usize) {
        if let Some(vs) = self.variants.get_mut(variant)
            && vs.contains(segment_index)
        {
            trace!(
                variant,
                segment_index, "stream_index::on_segment_invalidated"
            );
            vs.remove(segment_index);
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
    pub fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool {
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
    pub fn total_bytes(&self, playlist: &dyn PlaylistAccess) -> u64 {
        let mut total = 0u64;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                total += self.variants[variant].get(seg_idx).map_or_else(
                    || playlist.segment_size(variant, seg_idx).unwrap_or(0),
                    SegmentData::total_len,
                );
            }
        }
        total
    }

    /// Effective total: max of committed watermark and estimated total.
    #[must_use]
    pub fn effective_total(&self, playlist: &dyn PlaylistAccess) -> u64 {
        self.max_end_offset().max(self.total_bytes(playlist))
    }

    /// Number of committed segments across all variants.
    #[must_use]
    pub fn num_committed(&self) -> usize {
        self.byte_map.iter().count()
    }

    /// Access per-variant storage.
    #[must_use]
    pub fn variant_segments(&self, variant: usize) -> Option<&VariantSegments> {
        self.variants.get(variant)
    }

    // --- Internal ---

    /// Compute the byte offset where `target_segment` starts in the virtual stream.
    ///
    /// Walks committed segments in layout order, summing their sizes.
    /// Uncommitted segments contribute 0 (they have no committed data).
    fn byte_offset_at(&self, target_segment: usize) -> u64 {
        let mut offset = 0u64;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                if seg_idx >= target_segment {
                    return offset;
                }
                if let Some(data) = self.variants[variant].get(seg_idx) {
                    offset += data.total_len();
                }
            }
        }
        offset
    }

    /// Rebuild `byte_map` entries for all committed segments from `from_segment` onward.
    ///
    /// This is the replacement for `cascade_contiguity` — when a segment's size
    /// changes (DRM reconciliation) or a segment is added/removed, all subsequent
    /// byte offsets shift to maintain contiguity.
    fn rebuild_byte_map_from(&mut self, from_segment: usize) {
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

        // Reinsert with correct offsets
        let mut offset = self.byte_offset_at(from_segment);
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                if seg_idx < from_segment {
                    continue;
                }
                if let Some(data) = self.variants[variant].get(seg_idx) {
                    let end = offset + data.total_len();
                    self.byte_map.insert(offset..end, (variant, seg_idx));
                    offset = end;
                }
            }
        }
    }
}

// LayoutIndex

impl LayoutIndex for StreamIndex {
    type Item = (usize, usize);

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
        vs.remove(0);
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
        idx.reset_to(5, 2);
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
        // seg 0 at 0..100, seg 2 at 100..200 (gap closed)
        assert_eq!(idx.max_end_offset(), 200);
    }

    #[kithara::test]
    fn on_segment_invalidated_unknown_is_noop() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.on_segment_invalidated(0, 5); // unknown
        assert_eq!(idx.max_end_offset(), 100);
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
}
