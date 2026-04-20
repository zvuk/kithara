//! Per-variant segment index with independent byte layouts.
//!
//! Each variant maintains its own `RangeMap<u64, SegmentIndex>` byte map.
//! The `layout_variant` field selects which byte map the decoder currently uses.
//! Switching variants never destroys data — it just changes the active map.
//!
//! Key properties:
//! - Per-variant `VariantSegments` stores actual (post-download) segment data
//! - Per-variant `RangeMap<u64, SegmentIndex>` provides O(log n) byte offset lookups
//! - `layout_variant` selects the active byte map for decoder reads
//! - `total_bytes` is derived (committed actual + estimated remaining), never formula-computed

use std::{collections::BTreeMap, ops::Range};

use kithara_assets::ResourceKey;
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

    /// Whether a segment is available (not invalidated).
    #[must_use]
    pub fn is_available(&self, segment_index: SegmentIndex) -> bool {
        self.segments
            .get(&segment_index)
            .is_some_and(|slot| slot.available)
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

    /// Highest segment index ever stored in this variant, regardless of
    /// current availability.
    ///
    /// Unlike [`last`], this includes segments whose `available` flag
    /// was flipped off by LRU eviction — the metadata still lives in
    /// the map. Callers use this to distinguish "peer fetched past
    /// this segment and the data was later evicted" from "peer never
    /// fetched this segment at all" (the stranded-layout case).
    #[must_use]
    pub fn max_stored_index(&self) -> Option<SegmentIndex> {
        self.segments.last_key_value().map(|(&k, _)| k)
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
/// Each variant has its own independent byte map. The `layout_variant` field
/// selects which byte map is active for the decoder. Switching variants never
/// destroys data — committed segments persist across all variants.
pub struct StreamIndex {
    /// Per-variant committed segment data, indexed by variant number.
    variants: Vec<VariantSegments>,
    /// Per-variant byte-offset maps. `variant_byte_maps[v]` maps
    /// `byte_offset → segment_index` for variant `v`. Only committed segments
    /// appear. Updated on every commit/reconcile/invalidate.
    variant_byte_maps: Vec<RangeMap<u64, SegmentIndex>>,
    /// Which variant's byte layout is currently active for the decoder.
    /// Changed when the decoder is recreated for a variant switch.
    layout_variant: VariantIndex,
    /// Expected total size per segment per variant (from `size_map` HEAD estimates).
    /// Used by `rebuild_variant_byte_map` to reserve space for uncommitted segments.
    expected_sizes: Vec<Vec<u64>>,
    /// Total number of segments in the playlist.
    num_segments: usize,
}

impl StreamIndex {
    /// Create a new stream index.
    ///
    /// `num_variants` — number of variants in the master playlist.
    /// `num_segments` — total number of segments per variant.
    /// Default layout variant: 0.
    #[must_use]
    pub fn new(num_variants: usize, num_segments: usize) -> Self {
        Self {
            variants: (0..num_variants).map(|_| VariantSegments::new()).collect(),
            variant_byte_maps: (0..num_variants).map(|_| RangeMap::new()).collect(),
            layout_variant: 0,
            expected_sizes: vec![Vec::new(); num_variants],
            num_segments,
        }
    }

    // Layout variant

    /// Which variant's byte layout is active for the decoder.
    #[must_use]
    pub fn layout_variant(&self) -> VariantIndex {
        self.layout_variant
    }

    /// Switch the active byte layout to a different variant.
    ///
    /// Called when the decoder is recreated for a variant switch.
    /// Does NOT destroy any data — all variants' byte maps persist.
    pub fn set_layout_variant(&mut self, variant: VariantIndex) {
        trace!(
            from = self.layout_variant,
            to = variant,
            "stream_index::set_layout_variant"
        );
        self.layout_variant = variant;
    }

    /// Total number of segments in the playlist.
    #[must_use]
    pub fn num_segments(&self) -> usize {
        self.num_segments
    }

    /// Number of variants.
    #[must_use]
    pub fn num_variants(&self) -> usize {
        self.variants.len()
    }

    // Mutations: expected sizes

    /// Set expected total sizes per segment (from `size_map` HEAD estimates).
    ///
    /// Called once after `calculate_size_map`. Enables `rebuild_variant_byte_map`
    /// to reserve correct offsets for not-yet-committed segments.
    pub fn set_expected_sizes(&mut self, variant: VariantIndex, sizes: Vec<u64>) {
        trace!(variant, count = sizes.len(), "set_expected_sizes");
        if variant < self.expected_sizes.len() {
            self.expected_sizes[variant] = sizes;
        }
        self.rebuild_variant_byte_map(variant, 0);
    }

    // Mutations: segment data

    /// Commit a downloaded segment.
    ///
    /// Updates the variant's byte map with correct byte offsets.
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
        self.rebuild_variant_byte_map(variant, segment_index);
    }

    /// DRM reconciliation: segment actual size differs from HEAD estimate.
    ///
    /// Replaces segment data and rebuilds byte map from that segment onward.
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
        self.rebuild_variant_byte_map(variant, segment_index);
    }

    /// Cache invalidation: remove a single segment.
    pub fn on_segment_invalidated(&mut self, variant: VariantIndex, segment_index: SegmentIndex) {
        if let Some(vs) = self.variants.get_mut(variant)
            && vs.contains(segment_index)
        {
            trace!(
                variant,
                segment_index, "stream_index::on_segment_invalidated"
            );
            vs.invalidate(segment_index);
            self.rebuild_variant_byte_map(variant, 0);
        }
    }

    // Reads

    /// Find the committed segment containing the given byte offset.
    ///
    /// Uses the active `layout_variant`'s byte map. O(log n).
    /// Returns `None` if offset is in a gap or past all committed data.
    #[must_use]
    pub fn find_at_offset(&self, offset: u64) -> Option<SegmentRef<'_>> {
        self.find_at_offset_in(self.layout_variant, offset)
    }

    /// Find a committed segment at offset in a specific variant's byte map.
    #[must_use]
    pub fn find_at_offset_in(&self, variant: VariantIndex, offset: u64) -> Option<SegmentRef<'_>> {
        let byte_map = self.variant_byte_maps.get(variant)?;
        let (range, &seg_idx) = byte_map.get_key_value(&offset)?;
        // Use get_stored (ignores `available` flag) so the reader can
        // resolve byte offsets for segments temporarily invalidated by
        // LRU eviction. Actual data availability is checked downstream
        // by range_ready / contains_range on the backend.
        let data = self.variants[variant].get_stored(seg_idx)?;
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

    /// Byte range of a committed segment in its variant's byte map.
    #[must_use]
    pub fn range_for(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<Range<u64>> {
        self.item_range((variant, segment_index))
    }

    /// First segment in the current layout.
    #[must_use]
    pub(crate) fn layout_floor_segment(&self) -> Option<(VariantIndex, SegmentIndex)> {
        if self.num_segments > 0 {
            Some((self.layout_variant, 0))
        } else {
            None
        }
    }

    /// Byte offset of a logical segment in the layout variant's byte space.
    #[must_use]
    pub(crate) fn layout_offset_for_segment(
        &self,
        target_segment: SegmentIndex,
        playlist: &dyn PlaylistAccess,
    ) -> Option<u64> {
        let variant = self.layout_variant;
        let mut offset = 0u64;
        for seg_idx in 0..target_segment.min(self.num_segments) {
            let seg_len = self
                .stored_segment(variant, seg_idx)
                .map(SegmentData::total_len)
                .or_else(|| {
                    self.expected_sizes
                        .get(variant)
                        .and_then(|v| v.get(seg_idx).copied())
                })
                .or_else(|| playlist.segment_size(variant, seg_idx))?;
            offset = offset.saturating_add(seg_len);
        }
        Some(offset)
    }

    /// Whether a committed segment is visible in its variant's byte map.
    #[must_use]
    pub fn is_visible(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.range_for(variant, segment_index).is_some()
    }

    /// Highest committed byte offset in the layout variant's byte map.
    #[must_use]
    pub fn max_end_offset(&self) -> u64 {
        self.max_end_offset_in(self.layout_variant)
    }

    /// Highest committed byte offset in a specific variant's byte map.
    #[must_use]
    pub fn max_end_offset_in(&self, variant: VariantIndex) -> u64 {
        self.variant_byte_maps
            .get(variant)
            .and_then(|bm| bm.iter().next_back())
            .map_or(0, |(range, _)| range.end)
    }

    /// Whether a segment has been committed.
    #[must_use]
    pub fn is_segment_loaded(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.variants
            .get(variant)
            .is_some_and(|vs| vs.contains(segment_index))
    }

    /// Whether a segment has stored data (regardless of `available` flag).
    ///
    /// Unlike `is_segment_loaded`, this returns true for segments that
    /// were temporarily invalidated by LRU eviction. Used by the
    /// scheduler to avoid re-downloading segments whose data is still
    /// on disk.
    #[must_use]
    pub fn has_stored_segment(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.stored_segment(variant, segment_index).is_some()
    }

    /// Whether the entire byte range is covered by committed segments
    /// in the layout variant's byte map.
    #[must_use]
    pub fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        self.variant_byte_maps[self.layout_variant]
            .gaps(range)
            .next()
            .is_none()
    }

    /// Derived total bytes for the layout variant.
    ///
    /// For each segment: committed → actual size, not committed → estimated size.
    /// Monotonically converges to the true value as segments download.
    #[must_use]
    pub(crate) fn total_bytes(&self, playlist: &dyn PlaylistAccess) -> u64 {
        let variant = self.layout_variant;
        let mut total = 0u64;
        for seg_idx in 0..self.num_segments {
            total += self.stored_segment(variant, seg_idx).map_or_else(
                || playlist.segment_size(variant, seg_idx).unwrap_or(0),
                SegmentData::total_len,
            );
        }
        total
    }

    /// Effective total: max of committed watermark and estimated total.
    #[must_use]
    pub(crate) fn effective_total(&self, playlist: &dyn PlaylistAccess) -> u64 {
        self.max_end_offset().max(self.total_bytes(playlist))
    }

    /// Map a byte offset to `(variant, segment_index)` in the layout variant.
    #[must_use]
    pub(crate) fn segment_for_offset(
        &self,
        offset: u64,
        playlist: &dyn PlaylistAccess,
    ) -> Option<(VariantIndex, SegmentIndex)> {
        if let Some(seg_ref) = self.find_at_offset(offset) {
            return Some((seg_ref.variant, seg_ref.segment_index));
        }

        let variant = self.layout_variant;
        let mut cursor = 0u64;
        for seg_idx in 0..self.num_segments {
            let seg_len = self
                .stored_segment(variant, seg_idx)
                .map(SegmentData::total_len)
                .or_else(|| {
                    self.expected_sizes
                        .get(variant)
                        .and_then(|v| v.get(seg_idx).copied())
                })
                .or_else(|| playlist.segment_size(variant, seg_idx))?;

            let end = cursor.saturating_add(seg_len);
            if offset < end {
                return Some((variant, seg_idx));
            }
            cursor = end;
        }

        None
    }

    /// Number of expected sizes stored (across all variants).
    #[must_use]
    pub fn expected_sizes_len(&self) -> usize {
        self.expected_sizes.iter().map(Vec::len).sum()
    }

    /// Number of committed segments in the layout variant's byte map.
    #[must_use]
    pub fn num_committed(&self) -> usize {
        self.variant_byte_maps[self.layout_variant].iter().count()
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
        let mut variants_to_rebuild: Vec<(VariantIndex, SegmentIndex)> = Vec::new();

        for (variant, vs) in self.variants.iter_mut().enumerate() {
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
            let mut first_removed: Option<SegmentIndex> = None;
            for segment_index in to_remove {
                first_removed =
                    Some(first_removed.map_or(segment_index, |current| current.min(segment_index)));
                vs.invalidate(segment_index);
            }
            if let Some(from_segment) = first_removed {
                variants_to_rebuild.push((variant, from_segment));
            }
        }

        for (variant, from_segment) in variants_to_rebuild {
            self.rebuild_variant_byte_map(variant, from_segment);
        }

        removed
    }

    // Internal

    /// Compute the byte offset where `target_segment` starts in a variant's byte space.
    ///
    /// Walks segments `0..target_segment`, summing committed or expected sizes.
    fn variant_byte_offset_at(&self, variant: VariantIndex, target_segment: SegmentIndex) -> u64 {
        let mut offset = 0u64;
        for seg_idx in 0..target_segment.min(self.num_segments) {
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
        offset
    }

    /// Rebuild a variant's byte map from `from_segment` onward.
    ///
    /// When a segment's size changes (DRM reconciliation) or a segment is
    /// added/removed, all subsequent byte offsets shift to maintain contiguity.
    fn rebuild_variant_byte_map(&mut self, variant: VariantIndex, from_segment: SegmentIndex) {
        // Compute starting offset before split borrows.
        let mut offset = self.variant_byte_offset_at(variant, from_segment);
        let num_segments = self.num_segments;

        // Split borrows: byte_map (mut) vs variants/expected_sizes (shared).
        let variants = &self.variants[variant];
        let expected_sizes = self.expected_sizes.get(variant);
        let byte_map = &mut self.variant_byte_maps[variant];

        // Remove old entries for segments >= from_segment
        let keys_to_remove: Vec<Range<u64>> = byte_map
            .iter()
            .filter(|&(_, &seg_idx)| seg_idx >= from_segment)
            .map(|(range, _)| range.clone())
            .collect();
        for range in keys_to_remove {
            byte_map.remove(range);
        }

        // Reinsert with correct offsets.
        // Insert entries for ALL segments (committed + estimated) so that
        // find_at_offset works even when segments commit out of order.
        // range_ready_from_segments validates actual data availability.
        for seg_idx in from_segment..num_segments {
            let stored_len = variants.get_stored(seg_idx).map(SegmentData::total_len);
            let total_len = stored_len
                .or_else(|| expected_sizes.and_then(|v| v.get(seg_idx).copied()))
                .unwrap_or(0);
            if total_len == 0 {
                continue;
            }
            let end = offset + total_len;
            byte_map.insert(offset..end, seg_idx);
            offset += total_len;
        }
    }
}

// Layout lookup helpers

impl StreamIndex {
    /// Resolve a byte offset to the (variant, segment) pair that owns it
    /// in the current layout variant.
    #[must_use]
    pub fn item_at_offset(&self, offset: u64) -> Option<(VariantIndex, SegmentIndex)> {
        self.variant_byte_maps[self.layout_variant]
            .get(&offset)
            .map(|&seg_idx| (self.layout_variant, seg_idx))
    }

    /// Byte range of `(variant, seg_idx)` in that variant's byte map.
    #[must_use]
    pub fn item_range(
        &self,
        (variant, seg_idx): (VariantIndex, SegmentIndex),
    ) -> Option<Range<u64>> {
        self.variant_byte_maps
            .get(variant)?
            .iter()
            .find(|&(_, &s)| s == seg_idx)
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

    // SegmentData

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

    // VariantSegments

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

    // StreamIndex: construction

    #[kithara::test]
    fn stream_index_new_defaults_to_variant_zero() {
        let idx = StreamIndex::new(4, 37);
        assert_eq!(idx.layout_variant(), 0);
        assert_eq!(idx.num_segments(), 37);
        assert_eq!(idx.num_variants(), 4);
    }

    // StreamIndex: set_layout_variant

    #[kithara::test]
    fn set_layout_variant_switches_active_map() {
        let mut idx = StreamIndex::new(4, 6);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(3, 0, make_segment_data(0, 900));

        // Default: variant 0
        assert_eq!(idx.max_end_offset(), 100);

        // Switch to variant 3
        idx.set_layout_variant(3);
        assert_eq!(idx.max_end_offset(), 900);

        // Switch back — variant 0's data is preserved
        idx.set_layout_variant(0);
        assert_eq!(idx.max_end_offset(), 100);
    }

    // StreamIndex: commit + byte_map

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
    fn commit_to_different_variants_independent() {
        let mut idx = StreamIndex::new(4, 6);
        // Variant 0: segments 0-2
        idx.commit_segment(0, 0, make_segment_data(623, 50000));
        idx.commit_segment(0, 1, make_segment_data(0, 50000));
        idx.commit_segment(0, 2, make_segment_data(0, 50000));
        // Variant 3: segments 0-2 (same indices, different variant)
        idx.commit_segment(3, 0, make_segment_data(624, 725000));
        idx.commit_segment(3, 1, make_segment_data(0, 723000));
        idx.commit_segment(3, 2, make_segment_data(0, 724000));

        // layout_variant = 0, only sees variant 0's byte_map
        let v0_total: u64 = 623 + 50000 + 50000 + 50000;
        assert_eq!(idx.max_end_offset(), v0_total);

        // Variant 3's byte_map is independent
        let v3_total: u64 = 624 + 725000 + 723000 + 724000;
        assert_eq!(idx.max_end_offset_in(3), v3_total);

        // Switch layout → variant 3 data visible
        idx.set_layout_variant(3);
        assert_eq!(idx.max_end_offset(), v3_total);
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

    // StreamIndex: find_at_offset

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
    fn find_at_offset_in_specific_variant() {
        let mut idx = StreamIndex::new(4, 6);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));
        idx.commit_segment(3, 0, make_segment_data(50, 700));

        // layout_variant = 0: only variant 0's segments visible
        let seg = idx.find_at_offset(50).expect("in v0");
        assert_eq!(seg.variant, 0);
        assert_eq!(seg.segment_index, 0);

        // find_at_offset_in: access variant 3 directly
        let seg = idx.find_at_offset_in(3, 50).expect("in v3");
        assert_eq!(seg.variant, 3);
        assert_eq!(seg.segment_index, 0);
        assert_eq!(seg.byte_offset, 0);
    }

    // StreamIndex: is_range_loaded

    #[kithara::test]
    fn is_range_loaded_contiguous() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));

        assert!(idx.is_range_loaded(&(0..200)));
        assert!(idx.is_range_loaded(&(50..150)));
        assert!(!idx.is_range_loaded(&(0..300)));
    }

    // StreamIndex: on_segment_invalidated

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
        // Invalidated segment stays in byte map for offset routing
        // (demand resolution needs the mapping). Actual data
        // availability is checked by range_ready / contains_range.
        let seg = idx
            .find_at_offset(150)
            .expect("invalidated segment keeps offset mapping for demand routing");
        assert_eq!(seg.segment_index, 1);
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
            !idx.is_segment_loaded(0, 0),
            "invalidated segment must not be loaded"
        );
        // Offset mapping preserved for demand routing.
        let seg = idx
            .find_at_offset(0)
            .expect("invalidated segment keeps offset mapping");
        assert_eq!(seg.segment_index, 0);
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
            !idx.is_segment_loaded(0, 1),
            "invalidated segment must not be loaded"
        );
        // Offset mapping preserved for demand routing.
        let seg = idx
            .find_at_offset(150)
            .expect("invalidated segment keeps offset mapping");
        assert_eq!(seg.segment_index, 1);
        let seg = idx
            .find_at_offset(250)
            .expect("segment 2 must keep its original offset");
        assert_eq!(seg.segment_index, 2);
        assert_eq!(seg.byte_offset, 200);
    }

    // StreamIndex: variant preservation

    #[kithara::test]
    fn variant_data_preserved_across_layout_switch() {
        let mut idx = StreamIndex::new(2, 4);
        // Commit to both variants
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));
        idx.commit_segment(1, 0, make_segment_data(0, 500));
        idx.commit_segment(1, 1, make_segment_data(0, 500));

        // Check variant 0
        assert_eq!(idx.max_end_offset(), 200);
        let seg = idx.find_at_offset(50).expect("v0 seg0");
        assert_eq!(seg.variant, 0);

        // Switch to variant 1 — variant 0's data untouched
        idx.set_layout_variant(1);
        assert_eq!(idx.max_end_offset(), 1000);
        let seg = idx.find_at_offset(50).expect("v1 seg0");
        assert_eq!(seg.variant, 1);

        // Switch back — variant 0 still complete
        idx.set_layout_variant(0);
        assert_eq!(idx.max_end_offset(), 200);
        assert_eq!(idx.find_at_offset(0).expect("v0").segment_index, 0);
        assert_eq!(idx.find_at_offset(100).expect("v0").segment_index, 1);
    }

    // Layout lookup helpers

    #[kithara::test]
    fn layout_index_item_at_offset() {
        let mut idx = StreamIndex::new(2, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 200));

        assert_eq!(idx.item_at_offset(50), Some((0, 0)));
        assert_eq!(idx.item_at_offset(150), Some((0, 1)));
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

    #[kithara::test]
    fn layout_index_item_range_cross_variant() {
        let mut idx = StreamIndex::new(2, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(1, 0, make_segment_data(0, 500));

        // Variant 0's range
        assert_eq!(idx.item_range((0, 0)), Some(0..100));
        // Variant 1's range (in its own byte space)
        assert_eq!(idx.item_range((1, 0)), Some(0..500));
    }

    // StreamIndex: sparse commit (gap between committed segments)

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
    fn commit_without_expected_sizes_collapses_gaps() {
        // Without expected_sizes, uncommitted segments have size 0.
        // This is the expected behavior — expected_sizes must be set first.
        let mut idx = StreamIndex::new(1, 10);

        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 8, make_segment_data(0, 120));

        // Without expected_sizes, segment 8 collapses to offset 100
        // (right after segment 0, gaps have size 0).
        let seg = idx.find_at_offset(100).expect("seg 8 after collapse");
        assert_eq!(seg.segment_index, 8);
        assert_eq!(seg.byte_offset, 100);
    }

    #[kithara::test]
    fn commit_with_expected_sizes_preserves_offsets() {
        let mut idx = StreamIndex::new(1, 10);
        idx.set_expected_sizes(0, vec![120; 10]);

        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 8, make_segment_data(0, 120));

        // segment 8 = 7 gaps * 120 + 100 (seg 0 actual) = 840 + 100 = 940
        // Wait: seg 0 actual=100, segs 1-7 expected=120 each = 840
        // offset = 100 + 840 = 940
        let seg = idx
            .find_at_offset(940)
            .expect("segment 8 with expected_sizes must be at 940");
        assert_eq!(seg.segment_index, 8);
        assert_eq!(seg.byte_offset, 940);
    }
}
