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

use std::{collections::BTreeMap, ops::Range, sync::Arc};

use kithara_assets::ResourceKey;
use rangemap::RangeMap;
use tracing::trace;
use url::Url;

use crate::{
    HlsError,
    ids::{SegmentIndex, VariantIndex},
    playlist::PlaylistAccess,
};

/// Actual data for a committed segment (post-download, post-DRM-decrypt).
///
/// Unlike `LoadedSegment`, this does NOT store `variant` or `byte_offset` —
/// variant is known from the per-variant storage, `byte_offset` is computed
/// by the compositor from the stream layout.
#[derive(Debug, Clone)]
pub struct SegmentData {
    /// Absolute URL of the init segment (fMP4 only).
    pub init_url: Option<Url>,
    /// Absolute URL of the media segment.
    pub media_url: Url,
    /// Size of the init segment in bytes (0 if no init).
    pub init_len: u64,
    /// Size of the media segment in bytes.
    pub media_len: u64,
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
    data: SegmentData,
    available: bool,
}

/// All committed segments for one HLS variant.
///
/// Keyed by segment index (natural HLS key), not byte offset.
/// Byte offsets are computed by `StreamIndex` from the stream layout.
#[derive(Debug, Default)]
pub struct VariantSegments {
    /// Permanent scheduler failures by segment index. Held alongside
    /// `segments` (not inside the slot) so failed-only entries do not
    /// fabricate a zero-sized `SegmentData` and trick `range_ready` into
    /// reporting "ready" for bytes that never arrived.
    failed: BTreeMap<SegmentIndex, Arc<HlsError>>,
    segments: BTreeMap<SegmentIndex, SegmentSlot>,
}

impl VariantSegments {
    /// Create empty variant storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remove all segments.
    pub fn clear(&mut self) {
        self.segments.clear();
    }

    /// Whether a segment is committed.
    #[must_use]
    pub fn contains(&self, segment_index: SegmentIndex) -> bool {
        self.segments
            .get(&segment_index)
            .is_some_and(|slot| slot.available)
    }

    /// Permanent failure recorded for a segment, if any.
    #[must_use]
    pub fn failed_at(&self, segment_index: SegmentIndex) -> Option<&Arc<HlsError>> {
        self.failed.get(&segment_index)
    }

    /// First committed segment (lowest index).
    #[must_use]
    pub fn first(&self) -> Option<(SegmentIndex, &SegmentData)> {
        self.iter().next()
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

    /// Insert or replace a committed segment.
    ///
    /// A successful commit clears any prior permanent-failure marker on
    /// the same segment — the data the reader was blocked on now exists.
    pub fn insert(&mut self, segment_index: SegmentIndex, data: SegmentData) {
        self.segments.insert(
            segment_index,
            SegmentSlot {
                data,
                available: true,
            },
        );
        self.failed.remove(&segment_index);
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

    /// Whether there are no committed segments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
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

    /// Last committed segment (highest index).
    #[must_use]
    pub fn last(&self) -> Option<(SegmentIndex, &SegmentData)> {
        self.iter().next_back()
    }

    /// Number of committed segments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Record a permanent failure for a segment.
    pub fn mark_failed(&mut self, segment_index: SegmentIndex, error: Arc<HlsError>) {
        self.failed.insert(segment_index, error);
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
}

/// Reference to a byte-map entry: either a committed segment or a
/// reserved slot whose bytes have not yet been fetched.
///
/// Returned by `StreamIndex::find_at_offset`. The byte map is the single
/// source of truth for "which segment owns this offset"; `data.is_none()`
/// means the slot is reserved via expected sizes but the segment has not
/// been committed yet. Ready-to-read checks must go through
/// `range_ready_from_segments` / backend contains-range, not through
/// `data.is_some()`.
#[derive(Debug)]
pub struct SegmentRef<'a> {
    /// Committed segment data (sizes, URLs). `None` for reserved slots.
    pub data: Option<&'a SegmentData>,
    /// Segment index within the variant's media playlist.
    pub segment_index: SegmentIndex,
    /// Variant index in the master playlist.
    pub variant: VariantIndex,
    /// Computed byte offset in the virtual stream.
    pub byte_offset: u64,
}

/// Single source of truth for HLS segment layout.
///
/// Each variant has its own independent byte map. The `layout_variant` field
/// selects which byte map is active for the decoder. Switching variants never
/// destroys data — committed segments persist across all variants.
pub struct StreamIndex {
    /// Which variant's byte layout is currently active for the decoder.
    /// Changed when the decoder is recreated for a variant switch.
    layout_variant: VariantIndex,
    /// Expected total size per segment per variant (from `size_map` HEAD estimates).
    /// Used by `rebuild_variant_byte_map` to reserve space for uncommitted segments.
    expected_sizes: Vec<Vec<u64>>,
    /// Per-variant byte-offset maps. `variant_byte_maps[v]` maps
    /// `byte_offset → segment_index` for variant `v`. Only committed segments
    /// appear. Updated on every commit/reconcile/invalidate.
    variant_byte_maps: Vec<RangeMap<u64, SegmentIndex>>,
    /// Per-variant committed segment data, indexed by variant number.
    variants: Vec<VariantSegments>,
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
            num_segments,
            variants: (0..num_variants).map(|_| VariantSegments::new()).collect(),
            variant_byte_maps: (0..num_variants).map(|_| RangeMap::new()).collect(),
            layout_variant: 0,
            expected_sizes: vec![Vec::new(); num_variants],
        }
    }

    /// Commit a downloaded segment (or replace it for DRM reconciliation).
    ///
    /// Inserts/updates the segment data in the variant and rebuilds the
    /// byte map from that segment onward. Callers that need
    /// DRM-reconciliation semantics simply call this again with the
    /// updated data — the second call replaces and re-rebuilds.
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

    /// Effective total: max of committed watermark and estimated total.
    #[must_use]
    pub(crate) fn effective_total(&self, playlist: &dyn PlaylistAccess) -> u64 {
        self.max_end_offset().max(self.total_bytes(playlist))
    }

    /// Number of expected sizes stored (across all variants).
    #[must_use]
    pub fn expected_sizes_len(&self) -> usize {
        self.expected_sizes.iter().map(Vec::len).sum()
    }

    /// Find the committed segment containing the given byte offset.
    ///
    /// Uses the active `layout_variant`'s byte map. O(log n).
    /// Returns `None` if offset is in a gap or past all committed data.
    #[must_use]
    pub fn find_at_offset(&self, offset: u64) -> Option<SegmentRef<'_>> {
        self.find_at_offset_in(self.layout_variant, offset)
    }

    /// Find the byte-map entry owning this offset in a specific variant.
    ///
    /// Returns reserved slots (expected-size placeholders) and committed
    /// segments alike. `data.is_none()` means the slot is reserved but
    /// the segment has not been committed; data availability is
    /// validated separately via `range_ready_from_segments`.
    #[must_use]
    pub fn find_at_offset_in(&self, variant: VariantIndex, offset: u64) -> Option<SegmentRef<'_>> {
        let byte_map = self.variant_byte_maps.get(variant)?;
        let (range, &seg_idx) = byte_map.get_key_value(&offset)?;
        let data = self
            .variants
            .get(variant)
            .and_then(|vs| vs.get_stored(seg_idx));
        Some(SegmentRef {
            variant,
            data,
            segment_index: seg_idx,
            byte_offset: range.start,
        })
    }

    /// Permanent failure recorded for the segment owning `offset` in the
    /// active layout variant, if any.
    ///
    /// Mirrors [`Self::find_at_offset`] — uses the same byte map so the
    /// reader and the scheduler agree on segment ownership at every offset.
    #[must_use]
    pub fn find_failed_at_offset(&self, offset: u64) -> Option<Arc<HlsError>> {
        self.find_failed_at_offset_in(self.layout_variant, offset)
    }

    /// Permanent failure recorded for the segment owning `offset` in
    /// `variant`, if any.
    #[must_use]
    pub fn find_failed_at_offset_in(
        &self,
        variant: VariantIndex,
        offset: u64,
    ) -> Option<Arc<HlsError>> {
        let byte_map = self.variant_byte_maps.get(variant)?;
        let (_range, &seg_idx) = byte_map.get_key_value(&offset)?;
        self.variants
            .get(variant)
            .and_then(|vs| vs.failed_at(seg_idx))
            .cloned()
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

    /// Whether a segment has been committed.
    #[must_use]
    pub fn is_segment_loaded(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.variants
            .get(variant)
            .is_some_and(|vs| vs.contains(segment_index))
    }

    /// Whether a committed segment is visible in its variant's byte map.
    #[must_use]
    pub fn is_visible(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.range_for(variant, segment_index).is_some()
    }

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
            let seg_len = self.segment_total_size(variant, seg_idx, playlist)?;
            offset = offset.saturating_add(seg_len);
        }
        Some(offset)
    }

    /// Which variant's byte layout is active for the decoder.
    #[must_use]
    pub fn layout_variant(&self) -> VariantIndex {
        self.layout_variant
    }

    /// Record a permanent failure for `(variant, segment_index)`.
    ///
    /// Only meant for errors the scheduler cannot recover from
    /// (`KeyProcessing`, `PlaylistParse`, `VariantNotFound`,
    /// `SegmentNotFound`, `InvalidUrl`). Network errors stay on the
    /// scheduler retry path. The reader's `wait_range` consults
    /// [`StreamIndex::find_failed_at_offset`] every iteration and exits
    /// with this error rather than spinning until the hang detector fires.
    pub fn mark_segment_failed(
        &mut self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        error: Arc<HlsError>,
    ) {
        if let Some(vs) = self.variants.get_mut(variant) {
            vs.mark_failed(segment_index, error);
        }
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

    /// Number of committed segments in the layout variant's byte map.
    #[must_use]
    pub fn num_committed(&self) -> usize {
        self.variant_byte_maps[self.layout_variant].iter().count()
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

    /// Byte range of a committed segment in its variant's byte map.
    #[must_use]
    pub fn range_for(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<Range<u64>> {
        self.item_range((variant, segment_index))
    }

    /// Rebuild a variant's byte map from `from_segment` onward.
    ///
    /// When a segment's size changes (DRM reconciliation) or a segment is
    /// added/removed, all subsequent byte offsets shift to maintain contiguity.
    fn rebuild_variant_byte_map(&mut self, variant: VariantIndex, from_segment: SegmentIndex) {
        let mut offset = self.variant_byte_offset_at(variant, from_segment);
        let num_segments = self.num_segments;

        let variants = &self.variants[variant];
        let expected_sizes = self.expected_sizes.get(variant);
        let byte_map = &mut self.variant_byte_maps[variant];

        let keys_to_remove: Vec<Range<u64>> = byte_map
            .iter()
            .filter(|&(_, &seg_idx)| seg_idx >= from_segment)
            .map(|(range, _)| range.clone())
            .collect();
        for range in keys_to_remove {
            byte_map.remove(range);
        }

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
            let seg_len = self.segment_total_size(variant, seg_idx, playlist)?;
            let end = cursor.saturating_add(seg_len);
            if offset < end {
                return Some((variant, seg_idx));
            }
            cursor = end;
        }

        None
    }

    /// Best-known total size of a segment in the given variant.
    ///
    /// Prefers stored actual bytes, then the HEAD-derived expected size,
    /// then the playlist's own estimate. Returns `None` when no source
    /// knows the size.
    fn segment_total_size(
        &self,
        variant: VariantIndex,
        seg_idx: SegmentIndex,
        playlist: &dyn PlaylistAccess,
    ) -> Option<u64> {
        self.stored_segment(variant, seg_idx)
            .map(SegmentData::total_len)
            .or_else(|| {
                self.expected_sizes
                    .get(variant)
                    .and_then(|v| v.get(seg_idx).copied())
            })
            .or_else(|| playlist.segment_size(variant, seg_idx))
    }

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

    #[must_use]
    pub(crate) fn stored_segment(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<&SegmentData> {
        self.variants.get(variant)?.get_stored(segment_index)
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

    /// Access per-variant storage.
    #[must_use]
    pub fn variant_segments(&self, variant: VariantIndex) -> Option<&VariantSegments> {
        self.variants.get(variant)
    }

    /// Find the visible segment containing the given byte offset.
    #[must_use]
    pub fn visible_segment_at(&self, offset: u64) -> Option<SegmentRef<'_>> {
        self.find_at_offset(offset)
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

    #[kithara::test]
    fn stream_index_new_defaults_to_variant_zero() {
        let idx = StreamIndex::new(4, 37);
        assert_eq!(idx.layout_variant(), 0);
        assert_eq!(idx.num_segments(), 37);
        assert_eq!(idx.num_variants(), 4);
    }

    #[kithara::test]
    fn set_layout_variant_switches_active_map() {
        let mut idx = StreamIndex::new(4, 6);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(3, 0, make_segment_data(0, 900));

        assert_eq!(idx.max_end_offset(), 100);

        idx.set_layout_variant(3);
        assert_eq!(idx.max_end_offset(), 900);

        idx.set_layout_variant(0);
        assert_eq!(idx.max_end_offset(), 100);
    }

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
        idx.commit_segment(0, 0, make_segment_data(623, 50000));
        idx.commit_segment(0, 1, make_segment_data(0, 50000));
        idx.commit_segment(0, 2, make_segment_data(0, 50000));
        idx.commit_segment(3, 0, make_segment_data(624, 725000));
        idx.commit_segment(3, 1, make_segment_data(0, 723000));
        idx.commit_segment(3, 2, make_segment_data(0, 724000));

        let v0_total: u64 = 623 + 50000 + 50000 + 50000;
        assert_eq!(idx.max_end_offset(), v0_total);

        let v3_total: u64 = 624 + 725000 + 723000 + 724000;
        assert_eq!(idx.max_end_offset_in(3), v3_total);

        idx.set_layout_variant(3);
        assert_eq!(idx.max_end_offset(), v3_total);
    }

    #[kithara::test]
    fn reconcile_segment_shifts_subsequent_offsets() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(100, 500));
        idx.commit_segment(0, 1, make_segment_data(0, 400));
        idx.commit_segment(0, 2, make_segment_data(0, 300));
        assert_eq!(idx.max_end_offset(), 1300);

        idx.commit_segment(0, 0, make_segment_data(90, 500));
        assert_eq!(idx.max_end_offset(), 1290);

        let seg1 = idx.find_at_offset(590).expect("seg 1 starts at 590");
        assert_eq!(seg1.segment_index, 1);
        assert_eq!(seg1.byte_offset, 590);
    }

    #[kithara::test]
    fn find_at_offset_returns_correct_segment() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(100, 500));
        idx.commit_segment(0, 1, make_segment_data(0, 400));
        idx.commit_segment(0, 2, make_segment_data(0, 300));

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

        let seg = idx.find_at_offset(50).expect("in v0");
        assert_eq!(seg.variant, 0);
        assert_eq!(seg.segment_index, 0);

        let seg = idx.find_at_offset_in(3, 50).expect("in v3");
        assert_eq!(seg.variant, 3);
        assert_eq!(seg.segment_index, 0);
        assert_eq!(seg.byte_offset, 0);
    }

    #[kithara::test]
    #[case::variant_0_slq(0_usize, 50_000_u64)]
    #[case::variant_1_smq(1_usize, 100_000_u64)]
    #[case::variant_2_shq(2_usize, 200_000_u64)]
    #[case::variant_3_slossless(3_usize, 500_000_u64)]
    fn find_at_offset_at_boundary_after_mixed_actual_and_expected_commits(
        #[case] variant: VariantIndex,
        #[case] baseline_size: u64,
    ) {
        let num_variants = 4;
        let num_segments = 37;
        let mut idx = StreamIndex::new(num_variants, num_segments);
        idx.set_layout_variant(variant);

        let expected_size = baseline_size + 1_500;
        idx.set_expected_sizes(variant, vec![expected_size; num_segments]);

        let actual_0_to_8: Vec<(u64, u64)> = (0..9_usize)
            .map(|i| {
                let jitter = (i64::try_from(i).expect("BUG: 0..9 fits in i64") - 4) * 2_000;
                let baseline_signed =
                    i64::try_from(baseline_size).expect("BUG: baseline_size fits in i64");
                let media =
                    u64::try_from((baseline_signed + jitter).max(1_000)).unwrap_or(baseline_size);
                if i == 0 { (627, media) } else { (0, media) }
            })
            .collect();
        let order_0_to_8: [usize; 9] = [3, 4, 2, 0, 1, 5, 6, 7, 8];
        for seg_idx in order_0_to_8 {
            let (init_len, media_len) = actual_0_to_8[seg_idx];
            idx.commit_segment(variant, seg_idx, make_segment_data(init_len, media_len));
        }

        let actual_22_to_26: Vec<(usize, u64, u64)> = (22..=26)
            .enumerate()
            .map(|(i, seg_idx)| {
                let jitter =
                    (i64::try_from(i).expect("BUG: enumerate index fits in i64") - 2) * 1_500;
                let baseline_signed =
                    i64::try_from(baseline_size).expect("BUG: baseline_size fits in i64");
                let media =
                    u64::try_from((baseline_signed + jitter).max(1_000)).unwrap_or(baseline_size);
                let init = if seg_idx == 22 { 627 } else { 0 };
                (seg_idx, init, media)
            })
            .collect();
        let commit_order: [usize; 5] = [23, 22, 24, 25, 26];
        for target_idx in commit_order {
            let (_, init_len, media_len) = *actual_22_to_26
                .iter()
                .find(|(seg_idx, _, _)| *seg_idx == target_idx)
                .expect("post-seek entry present");
            idx.commit_segment(variant, target_idx, make_segment_data(init_len, media_len));
        }

        let sum_0_to_8: u64 = actual_0_to_8.iter().map(|(init, media)| init + media).sum();
        let sum_9_to_21: u64 = expected_size * 13;
        let sum_22_to_26: u64 = actual_22_to_26
            .iter()
            .map(|(_, init, media)| init + media)
            .sum();
        let seg_27_start = sum_0_to_8 + sum_9_to_21 + sum_22_to_26;

        let seg = idx.find_at_offset(seg_27_start).unwrap_or_else(|| {
            panic!(
                "variant={variant} find_at_offset({seg_27_start}) returned None — \
                 byte_map is inconsistent after mixed actual/expected commits"
            )
        });
        assert_eq!(seg.segment_index, 27);
        assert_eq!(seg.byte_offset, seg_27_start);
    }

    #[kithara::test]
    fn find_at_offset_returns_reserved_entry_for_uncommitted_segment() {
        let mut idx = StreamIndex::new(1, 3);
        idx.set_expected_sizes(0, vec![100, 200, 300]);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 200));

        let range_2 = idx
            .range_for(0, 2)
            .expect("byte_map must reserve space for seg 2 from expected_sizes");
        assert_eq!(range_2, 300..600);

        let seg1 = idx.find_at_offset(299).expect("seg 1 at 299");
        assert_eq!(seg1.segment_index, 1);

        let seg2 = idx.find_at_offset(300).unwrap_or_else(|| {
            panic!(
                "find_at_offset(300) returned None — byte_map reserves seg 2 \
                 at [300..600), but find_at_offset_in filters it out via get_stored"
            )
        });
        assert_eq!(seg2.segment_index, 2);
        assert_eq!(seg2.byte_offset, 300);
    }

    #[kithara::test]
    fn item_at_offset_and_find_at_offset_agree_on_reserved_entry() {
        let mut idx = StreamIndex::new(1, 3);
        idx.set_expected_sizes(0, vec![100, 200, 300]);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 200));

        let via_item = idx.item_at_offset(300);
        let via_find = idx
            .find_at_offset(300)
            .map(|seg_ref| (seg_ref.variant, seg_ref.segment_index));

        assert_eq!(
            via_item, via_find,
            "item_at_offset and find_at_offset must agree on byte-map membership \
             (both go through the same variant_byte_maps entry); diverging here \
             means `current_layout_variant` and `reader_segment_floor` see a \
             different view than `item_at_offset` does"
        );
    }

    #[kithara::test]
    fn find_at_offset_returns_variant_for_reader_on_reserved_boundary() {
        let variant: VariantIndex = 2;
        let mut idx = StreamIndex::new(3, 5);
        idx.set_layout_variant(variant);
        idx.set_expected_sizes(variant, vec![1_000, 1_000, 1_000, 1_000, 1_000]);
        idx.commit_segment(variant, 0, make_segment_data(0, 1_000));
        idx.commit_segment(variant, 1, make_segment_data(0, 1_000));

        let seg = idx
            .find_at_offset(2_000)
            .expect("reader at boundary must resolve to reserved seg 2");
        assert_eq!(
            seg.variant, variant,
            "variant must be preserved at reserved boundary — otherwise \
             classify_seek cannot distinguish Preserve vs Reset"
        );
        assert_eq!(seg.segment_index, 2);
    }

    #[kithara::test]
    fn is_range_loaded_contiguous() {
        let mut idx = StreamIndex::new(1, 3);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));

        assert!(idx.is_range_loaded(&(0..200)));
        assert!(idx.is_range_loaded(&(50..150)));
        assert!(!idx.is_range_loaded(&(0..300)));
    }

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
        idx.on_segment_invalidated(0, 5);
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

    #[kithara::test]
    fn variant_data_preserved_across_layout_switch() {
        let mut idx = StreamIndex::new(2, 4);
        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 1, make_segment_data(0, 100));
        idx.commit_segment(1, 0, make_segment_data(0, 500));
        idx.commit_segment(1, 1, make_segment_data(0, 500));

        assert_eq!(idx.max_end_offset(), 200);
        let seg = idx.find_at_offset(50).expect("v0 seg0");
        assert_eq!(seg.variant, 0);

        idx.set_layout_variant(1);
        assert_eq!(idx.max_end_offset(), 1000);
        let seg = idx.find_at_offset(50).expect("v1 seg0");
        assert_eq!(seg.variant, 1);

        idx.set_layout_variant(0);
        assert_eq!(idx.max_end_offset(), 200);
        assert_eq!(idx.find_at_offset(0).expect("v0").segment_index, 0);
        assert_eq!(idx.find_at_offset(100).expect("v0").segment_index, 1);
    }

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

        assert_eq!(idx.item_range((0, 0)), Some(0..100));
        assert_eq!(idx.item_range((1, 0)), Some(0..500));
    }

    #[kithara::test]
    fn find_at_offset_with_gap_preserves_layout_offsets() {
        let mut idx = StreamIndex::new(1, 20);
        idx.set_expected_sizes(0, vec![200; 20]);
        for i in 0..4 {
            idx.commit_segment(0, i, make_segment_data(0, 200));
        }
        idx.commit_segment(0, 16, make_segment_data(0, 200));

        let seg = idx
            .find_at_offset(3200)
            .expect("segment 16 must be findable at its layout offset 3200");
        assert_eq!(seg.segment_index, 16);
        assert_eq!(seg.byte_offset, 3200);
    }

    #[kithara::test]
    fn commit_without_expected_sizes_collapses_gaps() {
        let mut idx = StreamIndex::new(1, 10);

        idx.commit_segment(0, 0, make_segment_data(0, 100));
        idx.commit_segment(0, 8, make_segment_data(0, 120));

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

        let seg = idx
            .find_at_offset(940)
            .expect("segment 8 with expected_sizes must be at 940");
        assert_eq!(seg.segment_index, 8);
        assert_eq!(seg.byte_offset, 940);
    }
}
