//! Segment index for random-access over HLS streams.
//!
//! Multi-variant index: each variant has its own segment index with independent byte offsets.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};

use url::Url;

/// Detect container format from segment URL extension.
fn detect_container_from_url(url: &Url) -> Option<DetectedContainer> {
    let path = url.path();
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "ts" | "m2ts" => Some(DetectedContainer::MpegTs),
        "mp4" | "m4s" | "m4a" | "m4v" => Some(DetectedContainer::Fmp4),
        _ => None,
    }
}

/// Encryption info for a segment (resolved key URL and IV).
/// Also used as context for decryption callback in `AssetStore`.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EncryptionInfo {
    pub key_url: Url,
    pub iv: [u8; 16],
}

/// Key for segment storage in BTreeMap.
/// Ordering: Init < Media(0) < Media(1) < ...
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentKey {
    Init,
    Media(usize),
}

impl PartialOrd for SegmentKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SegmentKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SegmentKey::Init, SegmentKey::Init) => Ordering::Equal,
            (SegmentKey::Init, SegmentKey::Media(_)) => Ordering::Less,
            (SegmentKey::Media(_), SegmentKey::Init) => Ordering::Greater,
            (SegmentKey::Media(a), SegmentKey::Media(b)) => a.cmp(b),
        }
    }
}

/// Internal segment data stored in the index.
#[derive(Debug, Clone)]
struct SegmentData {
    url: Url,
    len: u64,
    encryption: Option<EncryptionInfo>,
}

/// Entry in segment index: maps global byte range to segment file.
#[derive(Debug, Clone)]
pub struct SegmentEntry {
    pub global_start: u64,
    pub global_end: u64,
    pub url: Url,
    pub segment_index: usize,
    pub encryption: Option<EncryptionInfo>,
}

/// Index for a single variant.
/// Segments stored in BTreeMap with SegmentKey ordering: Init < Media(0) < Media(1) < ...
struct VariantIndex {
    segments: BTreeMap<SegmentKey, SegmentData>,
    /// Container format detected from init/first segment URL.
    detected_container: Option<DetectedContainer>,
    /// First media segment index (for ABR switch support).
    /// When ABR switches mid-stream, we start from segment N, not 0.
    first_media_segment: Option<usize>,
}

impl VariantIndex {
    fn new() -> Self {
        Self {
            segments: BTreeMap::new(),
            detected_container: None,
            first_media_segment: None,
        }
    }

    fn add(
        &mut self,
        url: Url,
        len: u64,
        segment_index: usize,
        encryption: Option<EncryptionInfo>,
    ) {
        let key = if segment_index == usize::MAX {
            SegmentKey::Init
        } else {
            // Track first media segment for ABR switch support.
            // Update if this is the first segment OR if we're adding an earlier segment
            // (which happens during seek backward after ABR switch).
            match self.first_media_segment {
                None => {
                    self.first_media_segment = Some(segment_index);
                }
                Some(first) if segment_index < first => {
                    tracing::debug!(
                        old_first = first,
                        new_first = segment_index,
                        "Updating first_media_segment (seek backward after ABR)"
                    );
                    self.first_media_segment = Some(segment_index);
                }
                _ => {}
            }
            SegmentKey::Media(segment_index)
        };

        // Detect container from init segment or first media segment
        if self.detected_container.is_none()
            && (key == SegmentKey::Init || self.first_media_segment == Some(segment_index))
        {
            self.detected_container = detect_container_from_url(&url);
        }

        self.segments.insert(
            key,
            SegmentData {
                url,
                len,
                encryption,
            },
        );
    }

    fn detected_container(&self) -> Option<DetectedContainer> {
        self.detected_container
    }

    /// Find segment by byte offset.
    /// INIT segment (if present) comes first at offset 0.
    /// Returns None if there are gaps in media segment indices.
    fn find(&self, offset: u64) -> Option<SegmentEntry> {
        // CRITICAL: Cumulative offset must account for segments BEFORE first_media_segment!
        //
        // When ABR switches mid-stream, first_media_segment != 0 (e.g., = 2).
        // Decoder expects segment 2 at offset 400000 (assuming 200KB per segment),
        // but cumulative offset started from 0, so segment 2 would be at 0..200000 (WRONG!).
        //
        // WORKAROUND: Use average segment size to estimate offset before first_media_segment.
        // TODO: Store absolute offset for each segment (requires SegmentMeta changes).
        let mut cumulative = if let Some(first) = self.first_media_segment {
            if first > 0 && !self.segments.is_empty() {
                // Calculate average segment size
                let total_len: u64 = self.segments.values().map(|d| d.len).sum();
                let avg_size = total_len / self.segments.len() as u64;
                // Estimate offset of segments before first_media_segment
                first as u64 * avg_size
            } else {
                0
            }
        } else {
            0
        };

        // Start from first_media_segment (supports ABR switch mid-stream)
        let mut expected_media_idx = self.first_media_segment.unwrap_or(0);

        for (key, data) in &self.segments {
            // Gap detection for media segments.
            if let SegmentKey::Media(idx) = key {
                if *idx > expected_media_idx {
                    return None;
                }
                expected_media_idx = idx + 1;
            }

            let global_start = cumulative;
            let global_end = cumulative + data.len;

            if offset >= global_start && offset < global_end {
                let segment_index = match key {
                    SegmentKey::Init => usize::MAX,
                    SegmentKey::Media(idx) => *idx,
                };
                return Some(SegmentEntry {
                    global_start,
                    global_end,
                    url: data.url.clone(),
                    segment_index,
                    encryption: data.encryption.clone(),
                });
            }
            cumulative = global_end;
        }
        None
    }

    fn total_len(&self) -> u64 {
        // Return maximum global_end among all segments.
        // This handles ABR case where only late segments are loaded (e.g., seg 2 at offset 400000).

        // Calculate cumulative offset from first_media_segment (same logic as find())
        let cumulative = if let Some(first) = self.first_media_segment {
            if first > 0 && !self.segments.is_empty() {
                let total_len: u64 = self.segments.values().map(|d| d.len).sum();
                let avg_size = total_len / self.segments.len() as u64;
                first as u64 * avg_size
            } else {
                0
            }
        } else {
            0
        };

        // Find max global_end by iterating segments in order
        let mut max_end = 0u64;
        let mut cum = cumulative;

        let mut sorted_segs: Vec<_> = self.segments.iter().collect();
        sorted_segs.sort_by_key(|(idx, _)| *idx);

        for (_idx, data) in sorted_segs {
            let global_end = cum + data.len;
            max_end = max_end.max(global_end);
            cum += data.len;
        }

        max_end
    }
}

/// Container format detected from segment URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedContainer {
    MpegTs,
    Fmp4,
}

/// Multi-variant segment index.
pub struct SegmentIndex {
    variants: HashMap<usize, VariantIndex>,
    finished: bool,
    error: Option<String>,
}

impl SegmentIndex {
    pub fn new() -> Self {
        Self {
            variants: HashMap::new(),
            finished: false,
            error: None,
        }
    }

    /// Add a segment to the variant's index.
    pub fn add(
        &mut self,
        url: Url,
        len: u64,
        variant: usize,
        segment_index: usize,
        encryption: Option<EncryptionInfo>,
    ) {
        self.variants
            .entry(variant)
            .or_insert_with(VariantIndex::new)
            .add(url, len, segment_index, encryption);
    }

    /// Get detected container for a variant (if available).
    pub fn detected_container(&self, variant: usize) -> Option<DetectedContainer> {
        self.variants
            .get(&variant)
            .and_then(|v| v.detected_container())
    }

    /// Find segment by offset for the specified variant.
    /// Returns owned SegmentEntry (computed on the fly).
    pub fn find(&self, offset: u64, variant: usize) -> Option<SegmentEntry> {
        self.variants.get(&variant)?.find(offset)
    }

    /// Find segment_index by offset in any loaded variant.
    /// Used as a hint for seek when the current variant doesn't have the needed segment.
    /// Returns None for init segments (usize::MAX) - they are loaded automatically on variant switch.
    pub fn find_segment_index_for_offset(&self, offset: u64) -> Option<usize> {
        for variant_idx in self.variants.values() {
            if let Some(entry) = variant_idx.find(offset) {
                // Don't return init segment index - init is handled by variant switch
                if entry.segment_index == usize::MAX {
                    continue;
                }
                return Some(entry.segment_index);
            }
        }
        None
    }

    /// Total length for the specified variant (or 0 if variant not loaded).
    pub fn total_len(&self, variant: usize) -> u64 {
        self.variants.get(&variant).map_or(0, |v| v.total_len())
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn set_finished(&mut self) {
        self.finished = true;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn make_url(variant: usize, segment: usize) -> Url {
        Url::parse(&format!(
            "http://example.com/variant_{}/segment_{}.ts",
            variant, segment
        ))
        .unwrap()
    }

    fn make_url_simple(segment: usize) -> Url {
        make_url(0, segment)
    }

    // ========================================================================
    // Тест Index-1: VariantIndex isolated - sequential add (PARAMETRIZED)
    // ========================================================================

    #[rstest]
    #[case(10, 200_000)] // 10 segments, 200KB each
    #[case(15, 150_000)] // 15 segments, 150KB each
    #[case(20, 100_000)] // 20 segments, 100KB each
    fn test_variant_index_sequential(#[case] num_segments: usize, #[case] segment_size: u64) {
        let mut idx = VariantIndex::new();

        // Добавляем segments 0..num_segments последовательно
        for seg in 0..num_segments {
            idx.add(make_url_simple(seg), segment_size, seg, None);
        }

        // Проверяем find() для каждого offset
        for seg in 0..num_segments {
            let offset = seg as u64 * segment_size;
            let entry = idx
                .find(offset)
                .unwrap_or_else(|| panic!("segment {} not found at offset {}", seg, offset));

            assert_eq!(
                entry.segment_index, seg,
                "segment {} has wrong segment_index",
                seg
            );
            assert_eq!(
                entry.global_start, offset,
                "segment {} has wrong global_start",
                seg
            );
            assert_eq!(
                entry.global_end,
                offset + segment_size,
                "segment {} has wrong global_end",
                seg
            );
        }

        // Проверяем first_media_segment
        assert_eq!(idx.first_media_segment, Some(0));
    }

    // ========================================================================
    // Тест Index-2: VariantIndex isolated - ABR switch (segments out of order, PARAMETRIZED)
    // ========================================================================

    #[rstest]
    #[case(4, 10, 200_000)] // Start at seg 4, total 10 segments
    #[case(5, 12, 150_000)] // Start at seg 5, total 12 segments
    #[case(7, 15, 100_000)] // Start at seg 7, total 15 segments
    fn test_variant_index_abr_out_of_order(
        #[case] start_seg: usize,
        #[case] total_segments: usize,
        #[case] segment_size: u64,
    ) {
        let mut idx = VariantIndex::new();

        // Сценарий ABR:
        // 1. Add segments start_seg..total_segments (ABR начал с mid-stream)
        for seg in start_seg..total_segments {
            idx.add(make_url_simple(seg), segment_size, seg, None);
        }

        assert_eq!(idx.first_media_segment, Some(start_seg));

        // 2. Seek backward: Add segments 0..start_seg
        for seg in 0..start_seg {
            idx.add(make_url_simple(seg), segment_size, seg, None);
        }

        // CRITICAL: first_media_segment должен обновиться на 0!
        assert_eq!(
            idx.first_media_segment,
            Some(0),
            "first_media_segment should update to 0 after seek backward"
        );

        // Проверяем cumulative offsets для ВСЕХ сегментов
        for seg in 0..total_segments {
            let offset = seg as u64 * segment_size;
            let entry = idx
                .find(offset)
                .unwrap_or_else(|| panic!("segment {} not found at offset {}", seg, offset));

            assert_eq!(
                entry.segment_index, seg,
                "segment {} has wrong segment_index",
                seg
            );
            assert_eq!(
                entry.global_start, offset,
                "segment {} has wrong global_start (expected {}, got {})",
                seg, offset, entry.global_start
            );
            assert_eq!(
                entry.global_end,
                offset + segment_size,
                "segment {} has wrong global_end",
                seg
            );
        }
    }

    // ========================================================================
    // Тест Index-3: SegmentIndex - multi-variant (PARAMETRIZED)
    // ========================================================================

    #[rstest]
    #[case(3, 10, 200_000)] // 3 variants, 10 segments each
    #[case(5, 12, 150_000)] // 5 variants, 12 segments each
    #[case(7, 15, 100_000)] // 7 variants, 15 segments each
    fn test_segment_index_multi_variant(
        #[case] num_variants: usize,
        #[case] segments_per_variant: usize,
        #[case] segment_size: u64,
    ) {
        let mut idx = SegmentIndex::new();

        // Добавляем сегменты для всех вариантов
        for variant in 0..num_variants {
            for seg in 0..segments_per_variant {
                idx.add(make_url(variant, seg), segment_size, variant, seg, None);
            }
        }

        // Проверяем find(offset, variant) для каждого варианта
        for variant in 0..num_variants {
            for seg in 0..segments_per_variant {
                let offset = seg as u64 * segment_size;
                let entry = idx
                    .find(offset, variant)
                    .unwrap_or_else(|| panic!("variant {} segment {} not found", variant, seg));

                assert_eq!(entry.segment_index, seg);
                assert_eq!(entry.global_start, offset);
                assert_eq!(entry.global_end, offset + segment_size);
            }
        }

        // find_segment_index_for_offset() должен найти в ЛЮБОМ варианте
        for seg in 0..segments_per_variant {
            let offset = seg as u64 * segment_size;
            assert_eq!(idx.find_segment_index_for_offset(offset), Some(seg));
        }
    }

    // ========================================================================
    // Тест Index-4: ABR switch scenario - incomplete variants (PARAMETRIZED)
    // ========================================================================

    #[rstest]
    #[case(3, 10, 2, 200_000)] // 3 variants, 10 total segs, switch at seg 2
    #[case(5, 15, 5, 150_000)] // 5 variants, 15 total segs, switch at seg 5
    #[case(7, 20, 8, 100_000)] // 7 variants, 20 total segs, switch at seg 8
    fn test_segment_index_abr_switch_incomplete(
        #[case] num_variants: usize,
        #[case] total_segments: usize,
        #[case] switch_at_seg: usize,
        #[case] segment_size: u64,
    ) {
        let mut idx = SegmentIndex::new();
        let old_variant = 0;
        let new_variant = num_variants - 1; // Switch to last variant

        // Полный сценарий ABR:
        // 1. variant=old_variant: add segments 0..switch_at_seg
        for seg in 0..switch_at_seg {
            idx.add(
                make_url(old_variant, seg),
                segment_size,
                old_variant,
                seg,
                None,
            );
        }

        // 2. ABR switch на new_variant
        // 3. variant=new_variant: add segments switch_at_seg..total_segments
        for seg in switch_at_seg..total_segments {
            idx.add(
                make_url(new_variant, seg),
                segment_size,
                new_variant,
                seg,
                None,
            );
        }

        // Проверяем что индекс правильно обрабатывает:

        // - Segments 0..switch_at_seg доступны только в old_variant
        for seg in 0..switch_at_seg {
            let offset = seg as u64 * segment_size;

            assert!(
                idx.find(offset, old_variant).is_some(),
                "segment {} should exist in old variant {}",
                seg,
                old_variant
            );

            assert!(
                idx.find(offset, new_variant).is_none(),
                "segment {} should NOT exist in new variant {}",
                seg,
                new_variant
            );
        }

        // - Segments switch_at_seg..total доступны только в new_variant
        for seg in switch_at_seg..total_segments {
            let offset = seg as u64 * segment_size;

            assert!(
                idx.find(offset, new_variant).is_some(),
                "segment {} should exist in new variant {}",
                seg,
                new_variant
            );

            // CRITICAL: Проверяем что global_start ПРАВИЛЬНЫЙ!
            // Это тест для бага cumulative offset!
            let entry = idx.find(offset, new_variant).unwrap();
            assert_eq!(
                entry.global_start, offset,
                "segment {} in new variant has WRONG global_start (expected {}, got {})",
                seg, offset, entry.global_start
            );
        }

        // - find_segment_index_for_offset() должен найти в ЛЮБОМ варианте
        for seg in 0..total_segments {
            let offset = seg as u64 * segment_size;
            assert_eq!(
                idx.find_segment_index_for_offset(offset),
                Some(seg),
                "find_segment_index_for_offset({}) failed",
                offset
            );
        }
    }

    // ========================================================================
    // Тест Index-5: Gap detection (PARAMETRIZED)
    // ========================================================================

    #[rstest]
    #[case(10, 5, 200_000)] // 10 total, gap at seg 5
    #[case(15, 8, 150_000)] // 15 total, gap at seg 8
    #[case(20, 12, 100_000)] // 20 total, gap at seg 12
    fn test_variant_index_gap_detection(
        #[case] total_segments: usize,
        #[case] gap_at: usize,
        #[case] segment_size: u64,
    ) {
        let mut idx = VariantIndex::new();

        // Add all segments EXCEPT gap_at
        for seg in 0..total_segments {
            if seg == gap_at {
                continue; // Skip - create gap
            }
            idx.add(make_url_simple(seg), segment_size, seg, None);
        }

        // find() should return None for offsets >= gap_at (gap detected)
        for seg in 0..gap_at {
            let offset = seg as u64 * segment_size;
            assert!(
                idx.find(offset).is_some(),
                "segment {} before gap should be found",
                seg
            );
        }

        for seg in gap_at..total_segments {
            let offset = seg as u64 * segment_size;
            assert!(
                idx.find(offset).is_none(),
                "segment {} after gap should NOT be found (gap detection)",
                seg
            );
        }
    }
}
