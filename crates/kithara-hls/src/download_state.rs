//! Download state: BTreeMap-based segment index with O(log n) offset lookups.
//!
//! `DownloadState` tracks loaded HLS segments ordered by byte offset, replacing
//! the old HashMap-based `SegmentIndex` with efficient range queries.
//! The `DownloadProgress` trait provides a testable read interface for
//! checking loaded segments and byte ranges.

use std::{
    collections::{BTreeMap, HashSet},
    ops::Range,
};

use rangemap::RangeSet;
use tracing::debug;
use url::Url;

// LoadedSegment

/// A segment that has been downloaded and placed in the virtual byte stream.
#[derive(Debug, Clone)]
pub struct LoadedSegment {
    /// Variant index in the master playlist.
    pub variant: usize,
    /// Segment index within the variant's media playlist.
    pub segment_index: usize,
    /// Byte offset of this segment in the virtual stream.
    pub byte_offset: u64,
    /// Size of the init segment in bytes (0 if no init).
    pub init_len: u64,
    /// Size of the media segment in bytes.
    pub media_len: u64,
    /// Absolute URL of the init segment (fMP4 only).
    pub init_url: Option<Url>,
    /// Absolute URL of the media segment.
    pub media_url: Url,
}

impl LoadedSegment {
    /// Total size of this segment (init + media).
    pub fn total_len(&self) -> u64 {
        self.init_len + self.media_len
    }

    /// Byte offset just past the end of this segment.
    pub fn end_offset(&self) -> u64 {
        self.byte_offset + self.total_len()
    }

    /// Whether the given byte offset falls within this segment.
    pub fn contains(&self, offset: u64) -> bool {
        offset >= self.byte_offset && offset < self.end_offset()
    }
}

// DownloadState

/// Index of loaded segments, ordered by byte offset for O(log n) lookups.
pub struct DownloadState {
    /// `byte_offset` -> segment (ordered, O(log n) lookup).
    entries: BTreeMap<u64, LoadedSegment>,
    /// (variant, `segment_index`) for O(1) "is loaded?" checks.
    loaded_keys: HashSet<(usize, usize)>,
    /// Byte ranges that have been loaded (for gap detection).
    loaded_ranges: RangeSet<u64>,
    /// Byte offset of the most recently pushed entry.
    last_offset: Option<u64>,
}

impl DownloadState {
    /// Create an empty download state.
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            loaded_keys: HashSet::new(),
            loaded_ranges: RangeSet::new(),
            last_offset: None,
        }
    }

    /// Add a loaded segment to the index.
    pub fn push(&mut self, segment: LoadedSegment) {
        let offset = segment.byte_offset;
        let end = segment.end_offset();

        debug!(
            variant = segment.variant,
            segment_index = segment.segment_index,
            byte_offset = offset,
            end,
            "download_state::push"
        );

        self.loaded_ranges.insert(offset..end);
        self.loaded_keys
            .insert((segment.variant, segment.segment_index));
        self.last_offset = Some(offset);
        self.entries.insert(offset, segment);
    }

    /// Find the segment containing the given byte offset (O(log n)).
    ///
    /// Uses `BTreeMap::range(..=offset)` to find the last entry at or before
    /// the offset, then checks if the offset falls within that segment.
    pub fn find_at_offset(&self, offset: u64) -> Option<&LoadedSegment> {
        self.entries
            .range(..=offset)
            .next_back()
            .map(|(_, seg)| seg)
            .filter(|seg| seg.contains(offset))
    }

    /// The most recently pushed segment.
    pub fn last(&self) -> Option<&LoadedSegment> {
        self.last_offset
            .and_then(|offset| self.entries.get(&offset))
    }

    /// First segment of the given variant by byte offset (`BTreeMap` is ordered).
    ///
    /// Used to find the start of a new variant after ABR switch -- this is where
    /// init data (ftyp/moov) lives for the new variant.
    pub fn first_segment_of_variant(&self, variant: usize) -> Option<&LoadedSegment> {
        self.entries.values().find(|seg| seg.variant == variant)
    }

    /// Number of loaded segments.
    pub fn num_entries(&self) -> usize {
        self.entries.len()
    }

    /// Highest end offset across all loaded segments (O(1) via `BTreeMap`).
    ///
    /// This is the "watermark" — the furthest byte position in the virtual stream.
    /// Used for throttling and EOF detection (replaces old `SegmentIndex::total_bytes()`).
    pub fn max_end_offset(&self) -> u64 {
        self.entries
            .values()
            .next_back()
            .map_or(0, LoadedSegment::end_offset)
    }

    /// Remove entries from other variants at or past the fence offset.
    ///
    /// Keeps all entries of `keep_variant` regardless of offset, and all entries
    /// from other variants that are strictly before the fence. Rebuilds
    /// `loaded_keys` and `loaded_ranges` from remaining entries.
    pub fn fence_at(&mut self, offset: u64, keep_variant: usize) {
        self.entries
            .retain(|_, seg| seg.byte_offset < offset || seg.variant == keep_variant);

        // Rebuild loaded_keys from remaining entries.
        self.loaded_keys.clear();
        for seg in self.entries.values() {
            self.loaded_keys.insert((seg.variant, seg.segment_index));
        }

        // Rebuild loaded_ranges from remaining entries.
        self.loaded_ranges.clear();
        for seg in self.entries.values() {
            self.loaded_ranges.insert(seg.byte_offset..seg.end_offset());
        }

        debug!(
            offset,
            keep_variant,
            remaining = self.entries.len(),
            "download_state::fence_at"
        );
    }
}

// DownloadProgress trait

/// Read-only interface for querying download progress.
#[cfg_attr(test, unimock::unimock(api = DownloadProgressMock))]
pub trait DownloadProgress: Send + Sync {
    /// Whether a specific segment has been loaded.
    fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool;

    /// Whether the entire byte range is loaded (no gaps).
    fn is_range_loaded(&self, range: &Range<u64>) -> bool;

    /// Total number of loaded bytes across all segments.
    #[cfg_attr(not(test), expect(dead_code))]
    fn total_loaded_bytes(&self) -> u64;
}

impl DownloadProgress for DownloadState {
    fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool {
        self.loaded_keys.contains(&(variant, segment_index))
    }

    fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        !self.loaded_ranges.gaps(range).any(|_| true)
    }

    fn total_loaded_bytes(&self) -> u64 {
        self.loaded_ranges.iter().map(|r| r.end - r.start).sum()
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    fn base_url() -> Url {
        Url::parse("https://cdn.example.com/audio/").unwrap()
    }

    fn make_segment(
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
        init_len: u64,
        media_len: u64,
    ) -> LoadedSegment {
        LoadedSegment {
            variant,
            segment_index,
            byte_offset,
            init_len,
            media_len,
            init_url: if init_len > 0 {
                Some(base_url().join("init.mp4").unwrap())
            } else {
                None
            },
            media_url: base_url()
                .join(&format!("segment-{segment_index}.m4s"))
                .unwrap(),
        }
    }

    // Test 1: push and find

    #[test]
    fn test_push_and_find() {
        let mut state = DownloadState::new();

        // Push 3 segments: [0..100), [100..300), [300..600)
        state.push(make_segment(0, 0, 0, 0, 100));
        state.push(make_segment(0, 1, 100, 0, 200));
        state.push(make_segment(0, 2, 300, 0, 300));

        assert_eq!(state.num_entries(), 3);

        // find_at_offset at start of each segment
        let seg = state.find_at_offset(0).unwrap();
        assert_eq!(seg.segment_index, 0);

        let seg = state.find_at_offset(100).unwrap();
        assert_eq!(seg.segment_index, 1);

        let seg = state.find_at_offset(300).unwrap();
        assert_eq!(seg.segment_index, 2);

        // find_at_offset inside segments
        let seg = state.find_at_offset(50).unwrap();
        assert_eq!(seg.segment_index, 0);

        let seg = state.find_at_offset(250).unwrap();
        assert_eq!(seg.segment_index, 1);

        let seg = state.find_at_offset(599).unwrap();
        assert_eq!(seg.segment_index, 2);

        // find_at_offset at boundaries (last byte of segment)
        let seg = state.find_at_offset(99).unwrap();
        assert_eq!(seg.segment_index, 0);

        let seg = state.find_at_offset(299).unwrap();
        assert_eq!(seg.segment_index, 1);

        // find_at_offset past end returns None
        assert!(state.find_at_offset(600).is_none());
        assert!(state.find_at_offset(1000).is_none());
    }

    // Test 2: is_range_loaded

    #[test]
    fn test_is_range_loaded() {
        let mut state = DownloadState::new();

        // Contiguous segments: [0..100), [100..200), [200..300)
        state.push(make_segment(0, 0, 0, 0, 100));
        state.push(make_segment(0, 1, 100, 0, 100));
        state.push(make_segment(0, 2, 200, 0, 100));

        // Full range loaded
        assert!(state.is_range_loaded(&(0..300)));

        // Partial ranges loaded
        assert!(state.is_range_loaded(&(0..100)));
        assert!(state.is_range_loaded(&(50..150)));
        assert!(state.is_range_loaded(&(100..200)));
        assert!(state.is_range_loaded(&(200..300)));

        // Range extending past loaded data
        assert!(!state.is_range_loaded(&(0..400)));
        assert!(!state.is_range_loaded(&(250..400)));

        // Completely missing range
        assert!(!state.is_range_loaded(&(500..600)));
    }

    // Test 3: is_segment_loaded

    #[test]
    fn test_is_segment_loaded() {
        let mut state = DownloadState::new();

        state.push(make_segment(0, 0, 0, 0, 100));
        state.push(make_segment(0, 1, 100, 0, 100));
        state.push(make_segment(3, 5, 200, 0, 100));

        // Loaded segments
        assert!(state.is_segment_loaded(0, 0));
        assert!(state.is_segment_loaded(0, 1));
        assert!(state.is_segment_loaded(3, 5));

        // Not loaded
        assert!(!state.is_segment_loaded(0, 2));
        assert!(!state.is_segment_loaded(0, 5));
        assert!(!state.is_segment_loaded(3, 0));
        assert!(!state.is_segment_loaded(99, 0));
    }

    // Test 4: last

    #[test]
    fn test_last() {
        let mut state = DownloadState::new();

        // Empty state
        assert!(state.last().is_none());

        // After first push
        state.push(make_segment(0, 0, 0, 0, 100));
        let last = state.last().unwrap();
        assert_eq!(last.variant, 0);
        assert_eq!(last.segment_index, 0);

        // After second push
        state.push(make_segment(0, 1, 100, 0, 100));
        let last = state.last().unwrap();
        assert_eq!(last.variant, 0);
        assert_eq!(last.segment_index, 1);

        // After variant switch
        state.push(make_segment(3, 14, 200, 50, 150));
        let last = state.last().unwrap();
        assert_eq!(last.variant, 3);
        assert_eq!(last.segment_index, 14);
        assert_eq!(last.init_len, 50);
    }

    // Test 5: fence_at

    #[test]
    fn test_fence_at() {
        let mut state = DownloadState::new();

        // V0 segments: [0..100), [100..200), [200..300)
        state.push(make_segment(0, 0, 0, 0, 100));
        state.push(make_segment(0, 1, 100, 0, 100));
        state.push(make_segment(0, 2, 200, 0, 100));

        // V3 segment: [300..400)
        state.push(make_segment(3, 0, 300, 0, 100));

        assert_eq!(state.num_entries(), 4);

        // Fence at 200, keep V3.
        // V0 entries at offset >= 200 removed (seg 2 at 200..300).
        // V0 entries before 200 kept (seg 0 at 0..100, seg 1 at 100..200).
        // V3 entries kept regardless.
        state.fence_at(200, 3);

        assert_eq!(state.num_entries(), 3);

        // V0 before fence kept
        assert!(state.is_segment_loaded(0, 0));
        assert!(state.is_segment_loaded(0, 1));

        // V0 at/past fence removed
        assert!(!state.is_segment_loaded(0, 2));

        // V3 kept
        assert!(state.is_segment_loaded(3, 0));

        // loaded_ranges rebuilt correctly
        assert!(state.is_range_loaded(&(0..200)));
        assert!(!state.is_range_loaded(&(200..300)));
        assert!(state.is_range_loaded(&(300..400)));

        // total_loaded_bytes reflects removals
        assert_eq!(state.total_loaded_bytes(), 300); // 200 (V0) + 100 (V3)
    }

    // Test 6: first_segment_of_variant

    #[test]
    fn test_first_segment_of_variant() {
        let mut state = DownloadState::new();

        // V0 at offset 0, 100
        state.push(make_segment(0, 0, 0, 0, 100));
        state.push(make_segment(0, 1, 100, 0, 100));

        // V3 at offset 200, 300
        state.push(make_segment(3, 5, 200, 0, 100));
        state.push(make_segment(3, 6, 300, 0, 100));

        // First of V0 is at offset 0
        let first_v0 = state.first_segment_of_variant(0).unwrap();
        assert_eq!(first_v0.byte_offset, 0);
        assert_eq!(first_v0.segment_index, 0);

        // First of V3 is at offset 200 (lowest in BTreeMap order)
        let first_v3 = state.first_segment_of_variant(3).unwrap();
        assert_eq!(first_v3.byte_offset, 200);
        assert_eq!(first_v3.segment_index, 5);

        // Missing variant returns None
        assert!(state.first_segment_of_variant(99).is_none());
    }

    // Test 7: find_at_offset BTreeMap performance

    #[test]
    fn test_find_at_offset_btree_performance() {
        let mut state = DownloadState::new();
        let segment_size: u64 = 1000;

        // Push 1000 contiguous segments
        for i in 0..1000 {
            state.push(make_segment(0, i, i as u64 * segment_size, 0, segment_size));
        }

        assert_eq!(state.num_entries(), 1000);

        // Verify finds correct segment at various positions
        // Start of first segment
        let seg = state.find_at_offset(0).unwrap();
        assert_eq!(seg.segment_index, 0);

        // Middle of stream
        let seg = state.find_at_offset(500_500).unwrap();
        assert_eq!(seg.segment_index, 500);

        // Last byte of last segment
        let seg = state.find_at_offset(999_999).unwrap();
        assert_eq!(seg.segment_index, 999);

        // Exact boundary between segments
        let seg = state.find_at_offset(500_000).unwrap();
        assert_eq!(seg.segment_index, 500);

        // One before boundary
        let seg = state.find_at_offset(499_999).unwrap();
        assert_eq!(seg.segment_index, 499);

        // Past end
        assert!(state.find_at_offset(1_000_000).is_none());

        // Spot check a few random positions
        let seg = state.find_at_offset(123_456).unwrap();
        assert_eq!(seg.segment_index, 123);

        let seg = state.find_at_offset(789_001).unwrap();
        assert_eq!(seg.segment_index, 789);
    }

    // Test 8: total_loaded_bytes

    #[test]
    fn test_total_loaded_bytes() {
        let mut state = DownloadState::new();

        assert_eq!(state.total_loaded_bytes(), 0);

        state.push(make_segment(0, 0, 0, 0, 100));
        assert_eq!(state.total_loaded_bytes(), 100);

        state.push(make_segment(0, 1, 100, 50, 150));
        assert_eq!(state.total_loaded_bytes(), 300); // 100 + (50 + 150)
    }

    // Test 9: LoadedSegment methods

    #[test]
    fn test_loaded_segment_methods() {
        let seg = make_segment(0, 0, 100, 50, 200);

        assert_eq!(seg.total_len(), 250);
        assert_eq!(seg.end_offset(), 350);

        assert!(!seg.contains(99));
        assert!(seg.contains(100));
        assert!(seg.contains(200));
        assert!(seg.contains(349));
        assert!(!seg.contains(350));
    }
}
