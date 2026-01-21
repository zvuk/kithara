//! Offset mapping for a single variant.

use std::collections::BTreeMap;
use url::Url;
use super::types::{SegmentMeta, EncryptionInfo};
use crate::playlist::EncryptionMethod;

/// Cached segment with computed global_offset.
#[derive(Debug, Clone)]
pub struct CachedSegment {
    pub segment_index: usize,
    pub global_offset: u64,
    pub len: u64,
    pub url: Url,
    pub encryption: Option<EncryptionInfo>,
}

/// Offset map for a single variant.
///
/// Maps segment_index → CachedSegment with global byte offsets.
/// Not thread-safe by itself (used inside DashMap in CachedLoader).
#[derive(Debug)]
pub struct OffsetMap {
    segments: BTreeMap<usize, CachedSegment>,
}

impl OffsetMap {
    pub fn new() -> Self {
        Self {
            segments: BTreeMap::new(),
        }
    }

    /// Insert loaded segment and compute global_offset.
    pub fn insert(&mut self, meta: SegmentMeta) {
        let global_offset = self.compute_global_offset(meta.segment_index, meta.len);

        let encryption = extract_encryption(&meta);

        self.segments.insert(
            meta.segment_index,
            CachedSegment {
                segment_index: meta.segment_index,
                global_offset,
                len: meta.len,
                url: meta.url,
                encryption,
            },
        );
    }

    /// Compute global_offset for new segment.
    fn compute_global_offset(&self, segment_index: usize, _len: u64) -> u64 {
        // Find previous segment (max segment_index < segment_index)
        if let Some((_, prev)) = self.segments.range(..segment_index).next_back() {
            // Check if previous segment is consecutive
            if prev.segment_index + 1 == segment_index {
                // Sequential - use exact offset
                return prev.global_offset + prev.len;
            } else {
                // Gap detected - estimate from segment_index
                let avg = self.average_segment_size().unwrap_or(200_000);
                return segment_index as u64 * avg;
            }
        }

        // No previous segment → estimate offset for gap
        if segment_index == 0 {
            return 0;
        }

        // Gap before this segment → estimate using avg_size
        let avg = self.average_segment_size().unwrap_or(200_000);
        segment_index as u64 * avg
    }

    /// Get cached segment by index.
    pub fn get(&self, segment_index: usize) -> Option<&CachedSegment> {
        self.segments.get(&segment_index)
    }

    /// Find segment covering byte offset.
    /// Returns (segment_index, local_offset).
    pub fn find_segment_for_offset(&self, offset: u64) -> Option<(usize, u64)> {
        for seg in self.segments.values() {
            let seg_end = seg.global_offset + seg.len;

            if offset >= seg.global_offset && offset < seg_end {
                let local_offset = offset - seg.global_offset;
                return Some((seg.segment_index, local_offset));
            }
        }
        None
    }

    /// Estimate segment index for byte offset (for unloaded segments).
    pub fn estimate_segment_index(&self, offset: u64) -> usize {
        let avg = self.average_segment_size().unwrap_or(200_000);
        (offset / avg) as usize
    }

    /// Get total loaded size.
    pub fn total_size(&self) -> u64 {
        self.segments.values()
            .map(|s| s.global_offset + s.len)
            .max()
            .unwrap_or(0)
    }

    fn average_segment_size(&self) -> Option<u64> {
        if self.segments.is_empty() {
            return None;
        }
        let total: u64 = self.segments.values().map(|s| s.len).sum();
        Some(total / self.segments.len() as u64)
    }

    /// Get number of loaded segments.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Get all loaded segment indices in sorted order.
    pub fn segment_indices(&self) -> Vec<usize> {
        self.segments.keys().copied().collect()
    }

    /// Check if segment is loaded.
    pub fn has_segment(&self, segment_index: usize) -> bool {
        self.segments.contains_key(&segment_index)
    }

    /// Get all cached segments in sorted order by segment_index.
    pub fn all_segments(&self) -> Vec<&CachedSegment> {
        self.segments.values().collect()
    }
}

/// Extract encryption info from segment metadata.
fn extract_encryption(meta: &SegmentMeta) -> Option<EncryptionInfo> {
    let key = meta.key.as_ref()?;

    if !matches!(key.method, EncryptionMethod::Aes128) {
        return None;
    }

    let key_info = key.key_info.as_ref()?;
    let key_uri = key_info.uri.as_ref()?;

    // Resolve key URL
    let key_url = if key_uri.starts_with("http://") || key_uri.starts_with("https://") {
        Url::parse(key_uri).ok()?
    } else {
        meta.url.join(key_uri).ok()?
    };

    // Derive IV
    let iv = key_info.iv.unwrap_or_else(|| {
        let mut iv = [0u8; 16];
        iv[8..].copy_from_slice(&meta.sequence.to_be_bytes());
        iv
    });

    Some(EncryptionInfo { key_url, iv })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::time::Duration;

    fn test_url(seg: usize) -> Url {
        Url::parse(&format!("http://test.com/seg{}.ts", seg))
            .expect("valid URL")
    }

    fn create_meta(idx: usize, len: u64) -> SegmentMeta {
        SegmentMeta {
            variant: 0,
            segment_index: idx,
            sequence: idx as u64,
            url: test_url(idx),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
            container: Some(crate::parsing::ContainerFormat::Ts),
        }
    }

    // OM-1: Sequential insert
    #[rstest]
    #[case(vec![(0, 200_000), (1, 210_000), (2, 195_000)])]
    #[case(vec![(0, 150_000), (1, 250_000), (2, 180_000), (3, 220_000)])]
    fn test_sequential_insert(#[case] segments: Vec<(usize, u64)>) {
        let mut map = OffsetMap::new();

        let mut expected_offset = 0u64;
        for (idx, len) in segments {
            map.insert(create_meta(idx, len));

            let cached = map.get(idx).expect("segment not found");
            assert_eq!(
                cached.global_offset, expected_offset,
                "segment {} has wrong offset", idx
            );
            assert_eq!(cached.len, len);

            expected_offset += len;
        }

        // Total size check
        assert_eq!(map.total_size(), expected_offset);
    }

    // OM-2: Out-of-order insert (ABR switch mid-stream - CRITICAL EDGE CASE)
    #[rstest]
    #[case(vec![4, 5, 6], vec![0, 1, 2, 3])]
    #[case(vec![7, 8, 9], vec![0, 1, 2])]
    fn test_out_of_order_insert(
        #[case] first_batch: Vec<usize>,
        #[case] second_batch: Vec<usize>,
    ) {
        let mut map = OffsetMap::new();
        let seg_size = 200_000u64;

        // Insert first batch (ABR started mid-stream)
        for idx in &first_batch {
            map.insert(create_meta(*idx, seg_size));
        }

        // Insert second batch (seek backward)
        for idx in &second_batch {
            map.insert(create_meta(*idx, seg_size));
        }

        // CRITICAL: ALL segments must have correct global_offset
        let all_indices: Vec<_> = first_batch.iter()
            .chain(second_batch.iter())
            .copied()
            .collect();

        for idx in all_indices {
            let cached = map.get(idx).expect(&format!("seg {} missing", idx));
            let expected_offset = idx as u64 * seg_size;

            assert_eq!(
                cached.global_offset, expected_offset,
                "segment {} has WRONG offset (expected {}, got {})",
                idx, expected_offset, cached.global_offset
            );
        }
    }

    // OM-3: Find by offset (boundaries)
    #[test]
    fn test_find_by_offset() {
        let mut map = OffsetMap::new();

        // seg 0: 0..200KB
        map.insert(create_meta(0, 200_000));
        // seg 1: 200KB..410KB
        map.insert(create_meta(1, 210_000));
        // seg 2: 410KB..605KB
        map.insert(create_meta(2, 195_000));

        // Within segments
        assert_eq!(map.find_segment_for_offset(0), Some((0, 0)));
        assert_eq!(map.find_segment_for_offset(100_000), Some((0, 100_000)));
        assert_eq!(map.find_segment_for_offset(199_999), Some((0, 199_999)));

        assert_eq!(map.find_segment_for_offset(200_000), Some((1, 0)));
        assert_eq!(map.find_segment_for_offset(300_000), Some((1, 100_000)));

        assert_eq!(map.find_segment_for_offset(410_000), Some((2, 0)));

        // Out of range
        assert_eq!(map.find_segment_for_offset(700_000), None);
    }

    // OM-4: Gap detection
    #[rstest]
    #[case(vec![0, 1, 3, 4], 2)]  // Missing seg 2
    #[case(vec![0, 2, 3], 1)]     // Missing seg 1
    fn test_gap_detection(#[case] loaded: Vec<usize>, #[case] gap_at: usize) {
        let mut map = OffsetMap::new();
        let seg_size = 200_000u64;

        for idx in loaded {
            map.insert(create_meta(idx, seg_size));
        }

        // Offsets before gap should work
        for idx in 0..gap_at {
            let offset = idx as u64 * seg_size;
            assert!(
                map.find_segment_for_offset(offset).is_some(),
                "segment {} before gap should be found", idx
            );
        }

        // Gap offset should fail
        let gap_offset = gap_at as u64 * seg_size;
        assert!(
            map.find_segment_for_offset(gap_offset).is_none(),
            "gap at segment {} should return None", gap_at
        );
    }

    // OM-5: Estimation
    #[test]
    fn test_estimate_segment_index() {
        let mut map = OffsetMap::new();

        // Load a few segments to calculate avg
        for i in 0..3 {
            map.insert(create_meta(i, 200_000));
        }

        // Estimate for unloaded segment 10
        let estimated = map.estimate_segment_index(2_000_000);
        assert_eq!(estimated, 10);
    }

    // OM-6: VBR segments (variable bitrate)
    #[test]
    fn test_vbr_segments() {
        let mut map = OffsetMap::new();
        let sizes = vec![150_000, 250_000, 180_000, 220_000];

        for (idx, &size) in sizes.iter().enumerate() {
            map.insert(create_meta(idx, size));
        }

        // Verify cumulative offsets
        let mut cumulative = 0u64;
        for (idx, &size) in sizes.iter().enumerate() {
            let cached = map.get(idx).expect("segment not found");
            assert_eq!(cached.global_offset, cumulative);
            cumulative += size;
        }
    }

    // OM-7: Single segment
    #[test]
    fn test_single_segment() {
        let mut map = OffsetMap::new();
        map.insert(create_meta(0, 200_000));

        assert_eq!(map.find_segment_for_offset(0), Some((0, 0)));
        assert_eq!(map.find_segment_for_offset(199_999), Some((0, 199_999)));
        assert_eq!(map.find_segment_for_offset(200_000), None);
    }

    // OM-8: Empty map
    #[test]
    fn test_empty_map() {
        let map = OffsetMap::new();

        assert_eq!(map.find_segment_for_offset(0), None);
        assert_eq!(map.total_size(), 0);
        assert_eq!(map.estimate_segment_index(1_000_000), 5); // Default 200KB avg
    }
}
