# SegmentIndex Redesign: PlaylistState + DownloadState + Traits

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace scattered segment data (SegmentIndex, SegmentMetadata, variant_metadata, SegmentEntry, SegmentMeta) with a clean architecture: `PlaylistState` (immutable playlist model), `DownloadState` (mutable download progress with BTreeMap), and two traits (`PlaylistAccess`, `DownloadProgress`) for testable contracts via unimock.

**Architecture:** Three layers — (1) `PlaylistState` holds parsed playlist data + size maps, owned by FetchManager, shared via Arc; (2) `DownloadState` replaces SegmentIndex with BTreeMap<u64, LoadedSegment> for O(log n) offset lookup; (3) traits formalize the read API so HlsSource and HlsDownloader can be tested against mocks. SharedSegments simplified: `had_midstream_switch` and `expected_total_length` become atomics, `variant_metadata` removed.

**Tech Stack:** Rust, parking_lot, rangemap, crossbeam-queue, unimock, BTreeMap, AtomicU64/AtomicBool

---

## Overview of Phases

| Phase | What | Risk | Files touched |
|-------|------|------|---------------|
| 1 | Create `PlaylistState` + `PlaylistAccess` trait | None (additive) | 2 new files |
| 2 | Create `DownloadState` + `DownloadProgress` trait | None (additive) | 1 new file |
| 3 | Wire `PlaylistState` into FetchManager | Low (internal) | fetch.rs, inner.rs |
| 4 | Migrate HlsDownloader to new types | Medium | downloader.rs |
| 5 | Migrate HlsSource to new types | Medium | source.rs |
| 6 | Cleanup old types, run full test suite | Low | source.rs, downloader.rs |

**Safety net:** All integration tests use only public API (`Stream<Hls>`, `Audio<Stream<Hls>>`). No integration test references `SegmentIndex`, `SegmentEntry`, `SharedSegments`, or `HlsDownloader` directly. Unit tests in source.rs will be rewritten.

---

## Phase 1: PlaylistState + PlaylistAccess Trait

### Task 1.1: Create `playlist_state.rs` with types and trait

**Files:**
- Create: `crates/kithara-hls/src/playlist_state.rs`
- Modify: `crates/kithara-hls/src/lib.rs` (add module)

**Step 1: Write the failing test**

In `crates/kithara-hls/src/playlist_state.rs`, write:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use kithara_stream::{AudioCodec, ContainerFormat};
    use url::Url;

    fn test_variant(id: usize, num_segments: usize) -> VariantState {
        let segments: Vec<SegmentState> = (0..num_segments)
            .map(|i| SegmentState {
                index: i,
                url: Url::parse(&format!("http://test.com/v{id}/seg{i}.m4s")).unwrap(),
                duration: Duration::from_secs(4),
                key: None,
            })
            .collect();

        VariantState {
            id,
            uri: Url::parse(&format!("http://test.com/v{id}/index.m3u8")).unwrap(),
            bandwidth: Some(128_000),
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            init_url: Some(Url::parse(&format!("http://test.com/v{id}/init.mp4")).unwrap()),
            segments,
            size_map: None,
        }
    }

    #[test]
    fn test_playlist_state_basic_access() {
        let state = PlaylistState::new(vec![test_variant(0, 3), test_variant(1, 5)]);

        assert_eq!(state.num_variants(), 2);
        assert_eq!(state.num_segments(0), Some(3));
        assert_eq!(state.num_segments(1), Some(5));
        assert_eq!(state.num_segments(99), None);
    }

    #[test]
    fn test_playlist_state_variant_info() {
        let state = PlaylistState::new(vec![test_variant(0, 3)]);

        assert_eq!(state.variant_codec(0), Some(AudioCodec::AacLc));
        assert_eq!(state.variant_container(0), Some(ContainerFormat::Fmp4));
        assert!(state.segment_url(0, 0).is_some());
        assert!(state.segment_url(0, 3).is_none()); // out of bounds
        assert!(state.init_url(0).is_some());
    }

    #[test]
    fn test_size_map_not_set() {
        let state = PlaylistState::new(vec![test_variant(0, 3)]);

        assert!(!state.has_size_map(0));
        assert_eq!(state.total_variant_size(0), None);
        assert_eq!(state.segment_byte_offset(0, 0), None);
        assert_eq!(state.find_segment_at_offset(0, 500), None);
    }

    #[test]
    fn test_size_map_set_and_query() {
        let state = PlaylistState::new(vec![test_variant(0, 3)]);

        // Simulate HEAD request results: init=1000, seg0=5000, seg1=5000, seg2=5000
        let size_map = VariantSizeMap {
            init_size: 1000,
            segment_sizes: vec![5000, 5000, 5000],
            offsets: vec![0, 6000, 11000],  // seg0: 0 (init+media), seg1: 6000, seg2: 11000
            total: 16000,
        };
        state.set_size_map(0, size_map);

        assert!(state.has_size_map(0));
        assert_eq!(state.total_variant_size(0), Some(16000));
        assert_eq!(state.segment_byte_offset(0, 0), Some(0));
        assert_eq!(state.segment_byte_offset(0, 1), Some(6000));
        assert_eq!(state.segment_byte_offset(0, 2), Some(11000));

        // find_segment_at_offset
        assert_eq!(state.find_segment_at_offset(0, 0), Some(0));
        assert_eq!(state.find_segment_at_offset(0, 500), Some(0));
        assert_eq!(state.find_segment_at_offset(0, 5999), Some(0));
        assert_eq!(state.find_segment_at_offset(0, 6000), Some(1));
        assert_eq!(state.find_segment_at_offset(0, 11000), Some(2));
        assert_eq!(state.find_segment_at_offset(0, 16000), None); // past end
    }

    #[test]
    fn test_reconcile_segment_size() {
        let state = PlaylistState::new(vec![test_variant(0, 3)]);

        let size_map = VariantSizeMap {
            init_size: 1000,
            segment_sizes: vec![5000, 5000, 5000],
            offsets: vec![0, 6000, 11000],
            total: 16000,
        };
        state.set_size_map(0, size_map);

        // DRM: actual size smaller after decryption (4800 instead of 5000 for seg0)
        state.reconcile_segment_size(0, 0, 5800); // init(1000) + media(4800)

        assert_eq!(state.segment_byte_offset(0, 0), Some(0));
        assert_eq!(state.segment_byte_offset(0, 1), Some(5800)); // shifted
        assert_eq!(state.segment_byte_offset(0, 2), Some(10800)); // shifted
        assert_eq!(state.total_variant_size(0), Some(15800)); // reduced
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-hls playlist_state -- --nocapture`
Expected: FAIL — module and types don't exist yet.

**Step 3: Write minimal implementation**

```rust
//! Unified playlist state: all parsed playlist data + size maps.

use std::time::Duration;

use kithara_stream::{AudioCodec, ContainerFormat};
use parking_lot::RwLock;
use url::Url;

use crate::parsing::SegmentKey;

/// Per-segment parsed data (from media playlist).
#[derive(Debug, Clone)]
pub struct SegmentState {
    pub index: usize,
    pub url: Url,
    pub duration: Duration,
    pub key: Option<SegmentKey>,
}

/// Per-variant size information (from HEAD requests or download).
#[derive(Debug, Clone)]
pub struct VariantSizeMap {
    pub init_size: u64,
    /// Per-segment sizes (media only, without init for segments > 0).
    pub segment_sizes: Vec<u64>,
    /// Cumulative byte offsets for each segment in virtual stream.
    /// offsets[0] is always 0. offsets[i] = sum of sizes[0..i].
    pub offsets: Vec<u64>,
    /// Total byte length of variant's virtual stream.
    pub total: u64,
}

/// Per-variant parsed data (from master + media playlist).
#[derive(Debug, Clone)]
pub struct VariantState {
    pub id: usize,
    pub uri: Url,
    pub bandwidth: Option<u64>,
    pub codec: Option<AudioCodec>,
    pub container: Option<ContainerFormat>,
    pub init_url: Option<Url>,
    pub segments: Vec<SegmentState>,
    /// Filled after HEAD requests or download. None until then.
    pub size_map: Option<VariantSizeMap>,
}

/// Unified playlist state — single source of truth for all variant/segment data.
///
/// Created after parsing master + media playlists.
/// Size maps are set lazily (after HEAD requests or download).
/// Thread-safe: interior mutability via RwLock for size maps only.
pub struct PlaylistState {
    variants: Vec<RwLock<VariantState>>,
}

/// Trait for read-only access to playlist structure.
///
/// Enables mock-testing of components that depend on playlist data.
#[cfg_attr(test, unimock::unimock(api = PlaylistAccessMock))]
pub trait PlaylistAccess: Send + Sync {
    fn num_variants(&self) -> usize;
    fn num_segments(&self, variant: usize) -> Option<usize>;
    fn variant_codec(&self, variant: usize) -> Option<AudioCodec>;
    fn variant_container(&self, variant: usize) -> Option<ContainerFormat>;
    fn segment_url(&self, variant: usize, index: usize) -> Option<Url>;
    fn init_url(&self, variant: usize) -> Option<Url>;
    fn has_size_map(&self, variant: usize) -> bool;
    fn total_variant_size(&self, variant: usize) -> Option<u64>;
    fn segment_byte_offset(&self, variant: usize, index: usize) -> Option<u64>;
    fn find_segment_at_offset(&self, variant: usize, offset: u64) -> Option<usize>;
}

impl PlaylistState {
    pub fn new(variants: Vec<VariantState>) -> Self {
        Self {
            variants: variants.into_iter().map(RwLock::new).collect(),
        }
    }

    /// Set size map for a variant (after HEAD requests or download).
    pub fn set_size_map(&self, variant: usize, size_map: VariantSizeMap) {
        if let Some(v) = self.variants.get(variant) {
            v.write().size_map = Some(size_map);
        }
    }

    /// Update a segment's actual size (DRM: decrypted size differs from HEAD).
    /// Recalculates offsets for subsequent segments.
    pub fn reconcile_segment_size(&self, variant: usize, segment_index: usize, actual_total: u64) {
        let Some(v) = self.variants.get(variant) else {
            return;
        };
        let mut guard = v.write();
        let Some(ref mut sm) = guard.size_map else {
            return;
        };
        let Some(current_size) = sm.offsets.get(segment_index).copied() else {
            return;
        };

        // Update this segment's "total size in stream"
        let old_size = if segment_index + 1 < sm.offsets.len() {
            sm.offsets[segment_index + 1] - current_size
        } else {
            sm.total - current_size
        };

        if old_size == actual_total {
            return;
        }

        // Update segment_sizes entry
        if let Some(seg_size) = sm.segment_sizes.get_mut(segment_index) {
            let init_for_seg = if segment_index == 0 { sm.init_size } else { 0 };
            *seg_size = actual_total.saturating_sub(init_for_seg);
        }

        // Recalculate offsets from this point forward
        let mut offset = current_size + actual_total;
        for i in (segment_index + 1)..sm.offsets.len() {
            sm.offsets[i] = offset;
            let seg_size = sm.segment_sizes.get(i).copied().unwrap_or(0);
            offset += seg_size;
        }
        sm.total = offset;
    }
}

impl PlaylistAccess for PlaylistState {
    fn num_variants(&self) -> usize {
        self.variants.len()
    }

    fn num_segments(&self, variant: usize) -> Option<usize> {
        self.variants.get(variant).map(|v| v.read().segments.len())
    }

    fn variant_codec(&self, variant: usize) -> Option<AudioCodec> {
        self.variants.get(variant).and_then(|v| v.read().codec)
    }

    fn variant_container(&self, variant: usize) -> Option<ContainerFormat> {
        self.variants.get(variant).and_then(|v| v.read().container)
    }

    fn segment_url(&self, variant: usize, index: usize) -> Option<Url> {
        self.variants
            .get(variant)
            .and_then(|v| v.read().segments.get(index).map(|s| s.url.clone()))
    }

    fn init_url(&self, variant: usize) -> Option<Url> {
        self.variants
            .get(variant)
            .and_then(|v| v.read().init_url.clone())
    }

    fn has_size_map(&self, variant: usize) -> bool {
        self.variants
            .get(variant)
            .is_some_and(|v| v.read().size_map.is_some())
    }

    fn total_variant_size(&self, variant: usize) -> Option<u64> {
        self.variants
            .get(variant)
            .and_then(|v| v.read().size_map.as_ref().map(|sm| sm.total))
    }

    fn segment_byte_offset(&self, variant: usize, index: usize) -> Option<u64> {
        self.variants.get(variant).and_then(|v| {
            v.read()
                .size_map
                .as_ref()
                .and_then(|sm| sm.offsets.get(index).copied())
        })
    }

    fn find_segment_at_offset(&self, variant: usize, offset: u64) -> Option<usize> {
        self.variants.get(variant).and_then(|v| {
            let guard = v.read();
            let sm = guard.size_map.as_ref()?;
            if offset >= sm.total {
                return None;
            }
            // Binary search: find last offset <= target
            match sm.offsets.binary_search(&offset) {
                Ok(i) => Some(i),
                Err(i) => {
                    if i == 0 { None } else { Some(i - 1) }
                }
            }
        })
    }
}
```

**Step 4: Add module to lib.rs**

In `crates/kithara-hls/src/lib.rs`, add:
```rust
pub(crate) mod playlist_state;
```

**Step 5: Run tests to verify they pass**

Run: `cargo test -p kithara-hls playlist_state -- --nocapture`
Expected: All 5 tests PASS.

**Step 6: Run clippy + fmt**

Run: `cargo fmt --all && cargo clippy -p kithara-hls -- -D warnings`
Expected: Clean.

**Step 7: Commit**

```bash
git add crates/kithara-hls/src/playlist_state.rs crates/kithara-hls/src/lib.rs
git commit -m "feat(hls): add PlaylistState with PlaylistAccess trait

Single source of truth for parsed playlist data + size maps.
Replaces scattered VariantStream/SegmentMetadata/variant_metadata."
```

---

## Phase 2: DownloadState + DownloadProgress Trait

### Task 2.1: Create `download_state.rs` with types and trait

**Files:**
- Create: `crates/kithara-hls/src/download_state.rs`
- Modify: `crates/kithara-hls/src/lib.rs` (add module)

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn test_segment(
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
                Some(Url::parse(&format!("http://test.com/v{variant}/init.mp4")).unwrap())
            } else {
                None
            },
            media_url: Url::parse(&format!(
                "http://test.com/v{variant}/seg{segment_index}.m4s"
            ))
            .unwrap(),
        }
    }

    #[test]
    fn test_push_and_find() {
        let mut state = DownloadState::new();

        state.push(test_segment(0, 0, 0, 1000, 5000));
        state.push(test_segment(0, 1, 6000, 0, 5000));
        state.push(test_segment(0, 2, 11000, 0, 5000));

        assert_eq!(state.total_loaded_bytes(), 16000);

        let seg = state.find_at_offset(500).unwrap();
        assert_eq!(seg.segment_index, 0);

        let seg = state.find_at_offset(6000).unwrap();
        assert_eq!(seg.segment_index, 1);

        let seg = state.find_at_offset(15999).unwrap();
        assert_eq!(seg.segment_index, 2);

        assert!(state.find_at_offset(16000).is_none());
    }

    #[test]
    fn test_is_range_loaded() {
        let mut state = DownloadState::new();

        state.push(test_segment(0, 0, 0, 0, 100));
        state.push(test_segment(0, 1, 100, 0, 100));

        assert!(state.is_range_loaded(&(0..200)));
        assert!(state.is_range_loaded(&(50..150)));
        assert!(!state.is_range_loaded(&(0..300)));
    }

    #[test]
    fn test_is_segment_loaded() {
        let mut state = DownloadState::new();

        state.push(test_segment(0, 0, 0, 0, 100));
        state.push(test_segment(0, 1, 100, 0, 100));

        assert!(state.is_segment_loaded(0, 0));
        assert!(state.is_segment_loaded(0, 1));
        assert!(!state.is_segment_loaded(0, 2));
        assert!(!state.is_segment_loaded(1, 0));
    }

    #[test]
    fn test_last() {
        let mut state = DownloadState::new();
        assert!(state.last().is_none());

        state.push(test_segment(0, 0, 0, 0, 100));
        assert_eq!(state.last().unwrap().segment_index, 0);

        state.push(test_segment(0, 1, 100, 0, 100));
        assert_eq!(state.last().unwrap().segment_index, 1);

        state.push(test_segment(3, 14, 200, 1000, 5000));
        assert_eq!(state.last().unwrap().variant, 3);
        assert_eq!(state.last().unwrap().segment_index, 14);
    }

    #[test]
    fn test_fence_at() {
        let mut state = DownloadState::new();

        // V0: 0..100, 100..200, 200..300
        state.push(test_segment(0, 0, 0, 0, 100));
        state.push(test_segment(0, 1, 100, 0, 100));
        state.push(test_segment(0, 2, 200, 0, 100));

        // V3: 300..400
        state.push(test_segment(3, 3, 300, 0, 100));

        assert_eq!(state.num_entries(), 4);

        // Fence at 200, keep V3
        state.fence_at(200, 3);

        assert_eq!(state.num_entries(), 3);
        assert!(state.is_segment_loaded(0, 0));
        assert!(state.is_segment_loaded(0, 1));
        assert!(!state.is_segment_loaded(0, 2)); // removed
        assert!(state.is_segment_loaded(3, 3)); // kept

        assert!(state.is_range_loaded(&(0..200)));
        assert!(!state.is_range_loaded(&(200..300)));
        assert!(state.is_range_loaded(&(300..400)));
    }

    #[test]
    fn test_first_segment_of_variant() {
        let mut state = DownloadState::new();

        state.push(test_segment(0, 0, 0, 0, 100));
        state.push(test_segment(0, 1, 100, 0, 100));
        state.push(test_segment(3, 14, 200, 1000, 5000));

        let first_v0 = state.first_segment_of_variant(0).unwrap();
        assert_eq!(first_v0.segment_index, 0);
        assert_eq!(first_v0.byte_offset, 0);

        let first_v3 = state.first_segment_of_variant(3).unwrap();
        assert_eq!(first_v3.segment_index, 14);
        assert_eq!(first_v3.byte_offset, 200);

        assert!(state.first_segment_of_variant(99).is_none());
    }

    #[test]
    fn test_find_at_offset_btree_performance() {
        let mut state = DownloadState::new();

        // 1000 segments
        for i in 0..1000 {
            state.push(test_segment(0, i, i as u64 * 100, 0, 100));
        }

        // Should find via BTreeMap range query, not linear scan
        let seg = state.find_at_offset(50_000).unwrap();
        assert_eq!(seg.segment_index, 500);

        let seg = state.find_at_offset(99_999).unwrap();
        assert_eq!(seg.segment_index, 999);

        assert!(state.find_at_offset(100_000).is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p kithara-hls download_state -- --nocapture`
Expected: FAIL — module doesn't exist.

**Step 3: Write minimal implementation**

```rust
//! Download state: tracks which segments are loaded and their byte layout.

use std::{
    collections::{BTreeMap, HashSet},
    ops::Range,
};

use rangemap::RangeSet;
use tracing::debug;
use url::Url;

/// A loaded segment in the virtual byte stream.
#[derive(Debug, Clone)]
pub struct LoadedSegment {
    pub variant: usize,
    pub segment_index: usize,
    pub byte_offset: u64,
    pub init_len: u64,
    pub media_len: u64,
    pub init_url: Option<Url>,
    pub media_url: Url,
}

impl LoadedSegment {
    pub fn total_len(&self) -> u64 {
        self.init_len + self.media_len
    }

    pub fn end_offset(&self) -> u64 {
        self.byte_offset + self.total_len()
    }

    pub fn contains(&self, offset: u64) -> bool {
        offset >= self.byte_offset && offset < self.end_offset()
    }
}

/// Download progress state — what's been loaded and where.
///
/// Uses BTreeMap keyed by byte_offset for O(log n) offset lookups.
#[derive(Debug)]
pub struct DownloadState {
    /// byte_offset → LoadedSegment (ordered for range queries).
    entries: BTreeMap<u64, LoadedSegment>,
    /// (variant, segment_index) set for O(1) loaded checks.
    loaded_keys: HashSet<(usize, usize)>,
    /// Byte ranges that have been loaded.
    loaded_ranges: RangeSet<u64>,
    /// Key of most recently pushed entry.
    last_offset: Option<u64>,
}

/// Trait for querying download progress.
///
/// Enables mock-testing of components that depend on download state.
#[cfg_attr(test, unimock::unimock(api = DownloadProgressMock))]
pub trait DownloadProgress: Send + Sync {
    fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool;
    fn is_range_loaded(&self, range: &Range<u64>) -> bool;
    fn total_loaded_bytes(&self) -> u64;
}

impl Default for DownloadState {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadState {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            loaded_keys: HashSet::new(),
            loaded_ranges: RangeSet::new(),
            last_offset: None,
        }
    }

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

    /// Find loaded segment containing byte offset. O(log n) via BTreeMap.
    pub fn find_at_offset(&self, offset: u64) -> Option<&LoadedSegment> {
        // Find the entry with the largest byte_offset <= offset
        self.entries
            .range(..=offset)
            .next_back()
            .map(|(_, seg)| seg)
            .filter(|seg| seg.contains(offset))
    }

    pub fn last(&self) -> Option<&LoadedSegment> {
        self.last_offset.and_then(|off| self.entries.get(&off))
    }

    /// Find the first segment of the given variant (by lowest byte_offset).
    pub fn first_segment_of_variant(&self, variant: usize) -> Option<&LoadedSegment> {
        self.entries
            .values()
            .find(|seg| seg.variant == variant)
    }

    pub fn num_entries(&self) -> usize {
        self.entries.len()
    }

    /// Remove entries from other variants at or past the fence offset.
    /// Keeps entries before fence regardless of variant, and all entries of keep_variant.
    pub fn fence_at(&mut self, offset: u64, keep_variant: usize) {
        self.entries
            .retain(|_, seg| seg.byte_offset < offset || seg.variant == keep_variant);

        // Rebuild loaded_keys and loaded_ranges
        self.loaded_keys.clear();
        self.loaded_ranges.clear();
        for seg in self.entries.values() {
            self.loaded_keys.insert((seg.variant, seg.segment_index));
            self.loaded_ranges
                .insert(seg.byte_offset..seg.end_offset());
        }

        debug!(
            offset,
            keep_variant,
            remaining = self.entries.len(),
            "download_state::fence_at"
        );
    }
}

impl DownloadProgress for DownloadState {
    fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool {
        self.loaded_keys.contains(&(variant, segment_index))
    }

    fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        !self.loaded_ranges.gaps(range).any(|_| true)
    }

    fn total_loaded_bytes(&self) -> u64 {
        self.entries
            .values()
            .map(LoadedSegment::end_offset)
            .max()
            .unwrap_or(0)
    }
}
```

**Step 4: Add module to lib.rs**

In `crates/kithara-hls/src/lib.rs`, add:
```rust
pub(crate) mod download_state;
```

**Step 5: Run tests**

Run: `cargo test -p kithara-hls download_state -- --nocapture`
Expected: All 7 tests PASS.

**Step 6: Run clippy + fmt**

Run: `cargo fmt --all && cargo clippy -p kithara-hls -- -D warnings`
Expected: Clean.

**Step 7: Commit**

```bash
git add crates/kithara-hls/src/download_state.rs crates/kithara-hls/src/lib.rs
git commit -m "feat(hls): add DownloadState with DownloadProgress trait

BTreeMap-based download tracking with O(log n) offset lookup.
Replaces HashMap-based SegmentIndex."
```

---

## Phase 3: Wire PlaylistState into FetchManager

### Task 3.1: Build PlaylistState from parsed playlists

**Files:**
- Modify: `crates/kithara-hls/src/playlist_state.rs` (add builder)
- Modify: `crates/kithara-hls/src/fetch.rs` (hold Arc<PlaylistState>)
- Modify: `crates/kithara-hls/src/inner.rs` (create and pass PlaylistState)

**Step 1: Add `PlaylistState::from_parsed()` builder**

In `playlist_state.rs`, add:

```rust
impl PlaylistState {
    /// Build from parsed playlists.
    ///
    /// `master` provides variant info (bandwidth, codec, container).
    /// `media_playlists` provides per-variant segments and init segments.
    /// `resolve_url` resolves relative URLs against the media playlist URL.
    pub fn from_parsed(
        variants: &[crate::parsing::VariantStream],
        media_playlists: &[(Url, crate::parsing::MediaPlaylist)],
    ) -> Self {
        let variant_states: Vec<VariantState> = variants
            .iter()
            .enumerate()
            .map(|(i, vs)| {
                let (media_url, playlist) = &media_playlists[i];

                let init_url = playlist.init_segment.as_ref().and_then(|init| {
                    media_url.join(&init.uri).ok()
                });

                let segments = playlist
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(idx, seg)| {
                        let url = media_url.join(&seg.uri).unwrap_or_else(|_| media_url.clone());
                        SegmentState {
                            index: idx,
                            url,
                            duration: seg.duration,
                            key: seg.key.clone(),
                        }
                    })
                    .collect();

                VariantState {
                    id: i,
                    uri: media_url.clone(),
                    bandwidth: vs.bandwidth,
                    codec: vs.codec.as_ref().and_then(|c| c.audio_codec),
                    container: vs.codec.as_ref().and_then(|c| c.container)
                        .or(playlist.detected_container),
                    init_url,
                    segments,
                    size_map: None,
                }
            })
            .collect();

        Self::new(variant_states)
    }
}
```

Write a test:
```rust
#[test]
fn test_from_parsed_basic() {
    use crate::parsing::{
        MasterPlaylist, MediaPlaylist, MediaSegment, VariantId, VariantStream,
    };

    let variants = vec![VariantStream {
        id: VariantId(0),
        uri: "v0.m3u8".to_string(),
        bandwidth: Some(128_000),
        name: None,
        codec: None,
    }];

    let base = Url::parse("http://test.com/v0.m3u8").unwrap();
    let media = MediaPlaylist {
        segments: vec![
            MediaSegment {
                sequence: 0,
                variant_id: VariantId(0),
                uri: "seg0.m4s".to_string(),
                duration: Duration::from_secs(4),
                key: None,
            },
            MediaSegment {
                sequence: 1,
                variant_id: VariantId(0),
                uri: "seg1.m4s".to_string(),
                duration: Duration::from_secs(4),
                key: None,
            },
        ],
        target_duration: Some(Duration::from_secs(4)),
        init_segment: None,
        media_sequence: 0,
        end_list: true,
        current_key: None,
        detected_container: None,
        allow_cache: true,
    };

    let state = PlaylistState::from_parsed(&variants, &[(base, media)]);

    assert_eq!(state.num_variants(), 1);
    assert_eq!(state.num_segments(0), Some(2));
    assert!(state.segment_url(0, 0).is_some());
    assert!(state.init_url(0).is_none());
}
```

**Step 2: Add `Arc<PlaylistState>` to FetchManager**

In `fetch.rs`, add field:
```rust
pub struct FetchManager<N> {
    // ... existing fields ...
    playlist_state: Option<Arc<PlaylistState>>,
}
```

Add accessor:
```rust
pub fn playlist_state(&self) -> Option<&Arc<PlaylistState>> {
    self.playlist_state.as_ref()
}

pub fn set_playlist_state(&mut self, state: Arc<PlaylistState>) {
    self.playlist_state = Some(state);
}
```

Initialize to `None` in constructors.

**Step 3: Build PlaylistState in `inner.rs`**

In `Hls::create()`, after master playlist is loaded and before `build_pair()`:

```rust
// Load all media playlists eagerly for PlaylistState
let mut media_playlists = Vec::new();
for variant in &variants {
    let media_url = fetch_manager.resolve_url(&config.url, &variant.uri)?;
    let playlist = fetch_manager
        .media_playlist(&media_url, crate::parsing::VariantId(variant.id.0))
        .await?;
    media_playlists.push((media_url, playlist));
}

let playlist_state = Arc::new(PlaylistState::from_parsed(&variants, &media_playlists));
fetch_manager.set_playlist_state(Arc::clone(&playlist_state));
```

Pass `Arc<PlaylistState>` to `build_pair()`.

**Step 4: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests PASS (additive changes only).

**Step 5: Commit**

```bash
git commit -m "feat(hls): wire PlaylistState into FetchManager and Hls::create

Eagerly load all media playlists, build PlaylistState as single
source of truth for variant/segment structure."
```

---

## Phase 4: Migrate HlsDownloader to New Types

### Task 4.1: Replace variant_metadata + SegmentMetadata with PlaylistState

**Files:**
- Modify: `crates/kithara-hls/src/downloader.rs`
- Modify: `crates/kithara-hls/src/source.rs` (SharedSegments)

**Overview:** This is the biggest migration task. Done in sub-steps:

**Step 1: Add `Arc<PlaylistState>` to HlsDownloader and SharedSegments**

Add field to `HlsDownloader`:
```rust
pub(crate) playlist_state: Arc<PlaylistState>,
```

Add field to `SharedSegments`:
```rust
pub(crate) playlist_state: Arc<PlaylistState>,
```

Remove from `SharedSegments`:
```rust
// DELETE: pub(crate) variant_metadata: Mutex<HashMap<usize, Vec<SegmentMetadata>>>,
```

Update `build_pair()` to accept and pass `Arc<PlaylistState>`.

**Step 2: Migrate `calculate_variant_metadata` → `calculate_size_map`**

Replace the method that builds `Vec<SegmentMetadata>` with one that builds `VariantSizeMap` and stores it in `PlaylistState`:

```rust
async fn calculate_size_map(
    playlist_state: &PlaylistState,
    fetch: &Arc<DefaultFetchManager>,
    variant: usize,
) -> Result<(), HlsError> {
    if playlist_state.has_size_map(variant) {
        return Ok(());
    }

    let init_url = playlist_state.init_url(variant);
    let num_segments = playlist_state.num_segments(variant).unwrap_or(0);

    // HEAD for init
    let init_size = if let Some(ref url) = init_url {
        fetch.get_content_length(url).await.unwrap_or(0)
    } else {
        0
    };

    // HEAD for all media segments in parallel
    let media_futs: Vec<_> = (0..num_segments)
        .filter_map(|i| playlist_state.segment_url(variant, i))
        .map(|url| fetch.get_content_length(&url))
        .collect();
    let media_lengths = futures::future::join_all(media_futs).await;

    let mut offsets = Vec::with_capacity(num_segments);
    let mut segment_sizes = Vec::with_capacity(num_segments);
    let mut cumulative = 0u64;

    for (i, result) in media_lengths.into_iter().enumerate() {
        let media_len = result.unwrap_or(0);
        offsets.push(cumulative);
        let total_seg = if i == 0 { init_size + media_len } else { media_len };
        segment_sizes.push(media_len);
        cumulative += total_seg;
    }

    let size_map = VariantSizeMap {
        init_size,
        segment_sizes,
        offsets,
        total: cumulative,
    };

    playlist_state.set_size_map(variant, size_map);
    Ok(())
}
```

**Step 3: Migrate `ensure_variant_ready`**

Replace calls to `calculate_variant_metadata` with `calculate_size_map`.
Replace `self.shared.variant_metadata.lock().insert(variant, metadata)` with nothing (size_map is set on PlaylistState).
Replace `self.variant_lengths` lookups with `self.playlist_state.total_variant_size(variant)`.

**Step 4: Migrate `reconcile_metadata`**

Replace with:
```rust
fn reconcile_metadata(&self, variant: usize, segment_index: usize, actual_size: u64) {
    self.playlist_state.reconcile_segment_size(variant, segment_index, actual_size);
}
```

**Step 5: Remove `variants: Vec<VariantStream>` from HlsDownloader**

Replace `self.variants[dl.variant].codec...` with `self.playlist_state.variant_codec(dl.variant)`.

**Step 6: Remove `variant_lengths: HashMap<usize, u64>`**

All length queries go through `self.playlist_state.total_variant_size(variant)`.

**Step 7: Run tests**

Run: `cargo test -p kithara-hls && cargo test --workspace`
Expected: All PASS.

**Step 8: Commit**

```bash
git commit -m "refactor(hls): migrate HlsDownloader to PlaylistState

Remove variant_metadata, SegmentMetadata, variant_lengths.
Size maps calculated and stored in PlaylistState."
```

### Task 4.2: Replace SegmentIndex with DownloadState in SharedSegments

**Files:**
- Modify: `crates/kithara-hls/src/source.rs` (SharedSegments, HlsSource)
- Modify: `crates/kithara-hls/src/downloader.rs` (commit_segment)

**Step 1: Replace `Mutex<SegmentIndex>` with `Mutex<DownloadState>`**

In SharedSegments:
```rust
pub(crate) segments: Mutex<DownloadState>,  // was: Mutex<SegmentIndex>
pub(crate) expected_total_length: AtomicU64, // was: inside SegmentIndex
pub(crate) had_midstream_switch: AtomicBool, // was: inside SegmentIndex + HlsDownloader
```

**Step 2: Migrate `commit_segment` to build `LoadedSegment`**

```rust
let segment = LoadedSegment {
    variant: dl.variant,
    segment_index: dl.segment_index,
    byte_offset,
    init_len: actual_init_len,
    media_len: dl.media.len,
    init_url: dl.init_url,
    media_url: dl.media.url.clone(),
};
```

**Step 3: Migrate HlsSource methods**

- `wait_range`: use `segments.is_range_loaded()`, `segments.find_at_offset()`, `segments.total_loaded_bytes()`
- `read_at`: use `segments.find_at_offset()`, read using `seg.init_url` and `seg.media_url`
- `media_info`: use `self.playlist_state.variant_codec()` + `segments.last().variant`
- `find_segment_for_offset`: use `self.playlist_state.find_segment_at_offset()`
- `format_change_segment_range`: use `segments.first_segment_of_variant()`
- `len`: use `self.shared.expected_total_length.load()`

**Step 4: Migrate HlsDownloader reads**

- `should_throttle`: use `segments.total_loaded_bytes()`
- `plan`: use `segments.is_segment_loaded()`
- `poll_demand`: use `segments.is_segment_loaded()`
- `ensure_variant_ready`: use `self.shared.expected_total_length.store()` and `self.shared.had_midstream_switch.store()`

**Step 5: Run tests**

Run: `cargo test -p kithara-hls && cargo test --workspace`
Expected: All PASS.

**Step 6: Commit**

```bash
git commit -m "refactor(hls): replace SegmentIndex with DownloadState

BTreeMap-based O(log n) offset lookup. Atomic expected_total_length
and had_midstream_switch in SharedSegments."
```

---

## Phase 5: Simplify commit_segment offset logic

### Task 5.1: Unify byte_offset calculation

**Files:**
- Modify: `crates/kithara-hls/src/downloader.rs`

The current code has two modes for determining byte_offset (metadata vs cumulative). With PlaylistState, we can always use the size_map offset for the first variant, and cumulative for midstream switch:

```rust
let byte_offset = if is_midstream_switch || self.had_midstream_switch {
    self.byte_offset
} else {
    self.playlist_state
        .segment_byte_offset(dl.variant, dl.segment_index)
        .unwrap_or(self.byte_offset)
};
```

This is the same logic but with `PlaylistState` instead of `variant_metadata` — simpler because reconcile is handled by `PlaylistState::reconcile_segment_size`.

**Step 1: Verify current tests pass with this logic**

Run: `cargo test --workspace`

**Step 2: Commit**

```bash
git commit -m "refactor(hls): simplify commit_segment offset via PlaylistState"
```

---

## Phase 6: Cleanup

### Task 6.1: Remove old types

**Files:**
- Modify: `crates/kithara-hls/src/source.rs` (delete SegmentIndex, SegmentEntry)
- Modify: `crates/kithara-hls/src/downloader.rs` (delete SegmentMetadata)

**Step 1: Delete `SegmentIndex` struct and impl**

Remove lines 73-180 in source.rs (entire SegmentIndex).

**Step 2: Delete `SegmentEntry` struct and impl**

Remove lines 43-71 in source.rs.

**Step 3: Delete `SegmentMetadata` struct**

Remove lines 25-35 in downloader.rs.

**Step 4: Delete `had_midstream_switch` from HlsDownloader**

Use `self.shared.had_midstream_switch.load()` everywhere.

**Step 5: Rewrite unit tests**

Replace SegmentIndex tests with DownloadState tests (already written in Phase 2).
Replace SegmentEntry-based HlsSource tests with LoadedSegment-based tests.

**Step 6: Run full suite**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all --check`
Expected: All clean.

**Step 7: Run stress tests specifically**

Run: `cargo test --test integration stress_seek -- --nocapture`
Expected: All stress tests PASS.

**Step 8: Commit**

```bash
git commit -m "refactor(hls): remove SegmentIndex, SegmentEntry, SegmentMetadata

All segment data now flows through PlaylistState + DownloadState.
Single source of truth for playlist structure and download progress."
```

### Task 6.2: Run style check

Run: `bash scripts/ci/lint-style.sh`
Expected: Clean.

---

## Verification Checklist

After all phases:

- [ ] `cargo build --workspace` — compiles
- [ ] `cargo test --workspace` — all tests pass
- [ ] `cargo clippy --workspace -- -D warnings` — no warnings
- [ ] `cargo fmt --all --check` — formatted
- [ ] `bash scripts/ci/lint-style.sh` — style clean
- [ ] Stress tests pass: `stress_seek_random`, `stress_seek_abr_audio`, `stress_seek_audio_hls_wav`
- [ ] No `SegmentIndex`, `SegmentEntry`, or `SegmentMetadata` remain in codebase
- [ ] `variant_metadata` field removed from `SharedSegments`
- [ ] `had_midstream_switch` exists only as `AtomicBool` in `SharedSegments`
- [ ] `expected_total_length` exists only as `AtomicU64` in `SharedSegments`
- [ ] `PlaylistAccessMock` and `DownloadProgressMock` available via unimock
