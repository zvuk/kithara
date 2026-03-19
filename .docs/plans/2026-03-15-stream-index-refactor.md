# StreamIndex Refactor Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `DownloadState` with `StreamIndex` — per-variant segment storage + RangeMap compositor, fixing DRM total_bytes drift (484K discrepancy).

**Architecture:** New `StreamIndex` module with `VariantSegments` (per-variant `BTreeMap<usize, SegmentData>`), `variant_map: RangeMap<usize, usize>` for segment→variant assignment, `byte_map: RangeMap<u64, (usize, usize)>` for O(log n) byte offset lookups. `total_bytes` derived from committed actual + estimated remaining. `on_invalidated` callback in `StoreOptions` notifies on cache displacement.

**Tech Stack:** Rust, `rangemap` crate (already in workspace), `kithara_platform::RwLock`

**Design doc:** `.docs/refactor-stream-index.md`

---

## Chunk 1: Core StreamIndex Module

### Task 1: SegmentData and VariantSegments

**Files:**
- Create: `crates/kithara-hls/src/stream_index.rs`
- Modify: `crates/kithara-hls/src/lib.rs` (add `mod stream_index`)

- [ ] **Step 1: Write failing tests for SegmentData**

In `stream_index.rs`, add `#[cfg(test)] mod tests` with:

```rust
#[kithara::test]
fn segment_data_total_len() {
    let data = SegmentData {
        init_len: 623,
        media_len: 50000,
        init_url: None,
        media_url: Url::parse("https://cdn.example.com/seg0.m4s").unwrap(),
    };
    assert_eq!(data.total_len(), 50623);
}

#[kithara::test]
fn variant_segments_insert_and_get() {
    let mut vs = VariantSegments::new();
    let data = make_segment_data(100, 500);
    vs.insert(3, data);
    assert!(vs.contains(3));
    assert!(!vs.contains(4));
    assert_eq!(vs.get(3).unwrap().total_len(), 600);
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
}
```

- [ ] **Step 2: Run tests — verify they fail** (structs don't exist)

Run: `cargo test -p kithara-hls stream_index`

- [ ] **Step 3: Implement SegmentData + VariantSegments**

```rust
use std::collections::BTreeMap;
use url::Url;

/// Actual data for a committed segment (post-download, post-DRM-decrypt).
#[derive(Debug, Clone)]
pub struct SegmentData {
    pub init_len: u64,
    pub media_len: u64,
    pub init_url: Option<Url>,
    pub media_url: Url,
}

impl SegmentData {
    pub fn total_len(&self) -> u64 {
        self.init_len + self.media_len
    }
}

/// All committed segments for one HLS variant.
#[derive(Debug, Default)]
pub struct VariantSegments {
    segments: BTreeMap<usize, SegmentData>,
}

impl VariantSegments {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, segment_index: usize, data: SegmentData) {
        self.segments.insert(segment_index, data);
    }

    pub fn get(&self, segment_index: usize) -> Option<&SegmentData> {
        self.segments.get(&segment_index)
    }

    pub fn remove(&mut self, segment_index: usize) {
        self.segments.remove(&segment_index);
    }

    pub fn contains(&self, segment_index: usize) -> bool {
        self.segments.contains_key(&segment_index)
    }

    pub fn clear(&mut self) {
        self.segments.clear();
    }

    pub fn first(&self) -> Option<(usize, &SegmentData)> {
        self.segments.iter().next().map(|(&k, v)| (k, v))
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &SegmentData)> {
        self.segments.iter().map(|(&k, v)| (k, v))
    }
}
```

- [ ] **Step 4: Run tests — verify PASS**

Run: `cargo test -p kithara-hls stream_index`

- [ ] **Step 5: Commit**

`git add crates/kithara-hls/src/stream_index.rs crates/kithara-hls/src/lib.rs`
`git commit -m "feat(hls): add SegmentData and VariantSegments for StreamIndex"`

---

### Task 2: StreamIndex Core — Construction and variant_map

**Files:**
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Write failing tests for StreamIndex construction and switch_variant**

```rust
#[kithara::test]
fn stream_index_new_single_variant() {
    let idx = StreamIndex::new(4, 37); // 4 variants, 37 segments
    // Default: variant 0 owns all segments
    assert_eq!(idx.variant_at(0), Some(0));
    assert_eq!(idx.variant_at(36), Some(0));
    assert_eq!(idx.variant_at(37), None);
}

#[kithara::test]
fn switch_variant_appends_region() {
    let mut idx = StreamIndex::new(4, 37);
    idx.switch_variant(3, 3); // from seg 3 onward → variant 3
    assert_eq!(idx.variant_at(0), Some(0));
    assert_eq!(idx.variant_at(2), Some(0));
    assert_eq!(idx.variant_at(3), Some(3));
    assert_eq!(idx.variant_at(36), Some(3));
}

#[kithara::test]
fn switch_variant_truncates_tail() {
    let mut idx = StreamIndex::new(4, 37);
    idx.switch_variant(3, 3); // v0: 0..3, v3: 3..37
    idx.switch_variant(6, 0); // v0: 0..3, v3: 3..6, v0: 6..37
    assert_eq!(idx.variant_at(2), Some(0));
    assert_eq!(idx.variant_at(4), Some(3));
    assert_eq!(idx.variant_at(6), Some(0));
    assert_eq!(idx.variant_at(36), Some(0));
}

#[kithara::test]
fn reset_to_replaces_layout() {
    let mut idx = StreamIndex::new(4, 37);
    idx.switch_variant(3, 3);
    idx.reset_to(5, 2); // replace everything: v2 from seg 5
    assert_eq!(idx.variant_at(0), None); // before reset point
    assert_eq!(idx.variant_at(5), Some(2));
    assert_eq!(idx.variant_at(36), Some(2));
}
```

- [ ] **Step 2: Run tests — verify FAIL**

- [ ] **Step 3: Implement StreamIndex core**

```rust
use rangemap::RangeMap;

/// Unified segment index: per-variant storage + stream composition.
pub struct StreamIndex {
    variants: Vec<VariantSegments>,
    /// segment_index ranges → variant assignment.
    variant_map: RangeMap<usize, usize>,
    /// byte_offset ranges → (variant, segment_index). Committed segments only.
    byte_map: RangeMap<u64, (usize, usize)>,
    num_segments: usize,
}

impl StreamIndex {
    pub fn new(num_variants: usize, num_segments: usize) -> Self {
        let mut variant_map = RangeMap::new();
        if num_segments > 0 {
            variant_map.insert(0..num_segments, 0); // default: variant 0
        }
        Self {
            variants: (0..num_variants).map(|_| VariantSegments::new()).collect(),
            variant_map,
            byte_map: RangeMap::new(),
            num_segments,
        }
    }

    pub fn variant_at(&self, segment_index: usize) -> Option<usize> {
        self.variant_map.get(&segment_index).copied()
    }

    pub fn switch_variant(&mut self, segment_index: usize, variant: usize) {
        if segment_index < self.num_segments {
            self.variant_map.insert(segment_index..self.num_segments, variant);
        }
    }

    pub fn reset_to(&mut self, segment_index: usize, variant: usize) {
        self.variant_map.clear();
        if segment_index < self.num_segments {
            self.variant_map.insert(segment_index..self.num_segments, variant);
        }
    }
}
```

- [ ] **Step 4: Run tests — verify PASS**

- [ ] **Step 5: Commit**

`git commit -m "feat(hls): add StreamIndex with variant_map and switch/reset operations"`

---

### Task 3: StreamIndex Mutations — commit_segment, reconcile_segment

**Files:**
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[kithara::test]
fn commit_single_variant_builds_byte_map() {
    let mut idx = StreamIndex::new(1, 3);
    idx.commit_segment(0, 0, make_segment_data(623, 50000)); // init + media
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
    // v0 segments 0-2
    idx.commit_segment(0, 0, make_segment_data(623, 50000));
    idx.commit_segment(0, 1, make_segment_data(0, 50000));
    idx.commit_segment(0, 2, make_segment_data(0, 50000));
    // ABR switch to v3 at segment 3
    idx.switch_variant(3, 3);
    idx.commit_segment(3, 3, make_segment_data(624, 725000));
    idx.commit_segment(3, 4, make_segment_data(0, 723000));
    idx.commit_segment(3, 5, make_segment_data(0, 724000));

    let v0_total = 623 + 50000 + 50000 + 50000;
    let v3_total = 624 + 725000 + 723000 + 724000;
    assert_eq!(idx.max_end_offset(), v0_total + v3_total);
}

#[kithara::test]
fn reconcile_segment_shifts_subsequent_offsets() {
    let mut idx = StreamIndex::new(1, 3);
    idx.commit_segment(0, 0, make_segment_data(100, 500)); // 600
    idx.commit_segment(0, 1, make_segment_data(0, 400));   // 400
    idx.commit_segment(0, 2, make_segment_data(0, 300));   // 300
    assert_eq!(idx.max_end_offset(), 1300);

    // DRM reconciliation: segment 0 actual is smaller (590 instead of 600)
    idx.reconcile_segment(0, 0, make_segment_data(90, 500));
    assert_eq!(idx.max_end_offset(), 1290);
    // Segment 1 should now start at 590, not 600
    let seg1 = idx.find_at_offset(590).unwrap();
    assert_eq!(seg1.segment_index, 1);
}
```

- [ ] **Step 2: Run tests — verify FAIL**

- [ ] **Step 3: Implement commit_segment, reconcile_segment, rebuild_byte_map**

```rust
impl StreamIndex {
    /// Compute byte offset for segment_index by summing all preceding committed segments.
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

    pub fn commit_segment(
        &mut self,
        variant: usize,
        segment_index: usize,
        data: SegmentData,
    ) {
        let byte_offset = self.byte_offset_at(segment_index);
        let end = byte_offset + data.total_len();
        self.variants[variant].insert(segment_index, data);
        self.byte_map.insert(byte_offset..end, (variant, segment_index));
        // Rebuild byte_map for segments after this one (their offsets shift)
        self.rebuild_byte_map_from(segment_index + 1);
    }

    pub fn reconcile_segment(
        &mut self,
        variant: usize,
        segment_index: usize,
        new_data: SegmentData,
    ) {
        self.variants[variant].insert(segment_index, new_data);
        self.rebuild_byte_map_from(segment_index);
    }

    /// Rebuild byte_map entries for all committed segments from segment_index onward.
    fn rebuild_byte_map_from(&mut self, from_segment: usize) {
        // Remove old entries for segments >= from_segment
        let keys_to_remove: Vec<_> = self.byte_map.iter()
            .filter(|(_, &(_, seg_idx))| seg_idx >= from_segment)
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

    pub fn max_end_offset(&self) -> u64 {
        self.byte_map.iter().last().map_or(0, |(range, _)| range.end)
    }

    pub fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool {
        self.variants.get(variant)
            .is_some_and(|vs| vs.contains(segment_index))
    }
}
```

- [ ] **Step 4: Run tests — verify PASS**

- [ ] **Step 5: Commit**

`git commit -m "feat(hls): StreamIndex commit_segment and reconcile_segment with byte_map"`

---

### Task 4: StreamIndex Reads — find_at_offset, is_range_loaded, on_segment_invalidated

**Files:**
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Write failing tests**

```rust
/// Returned by find_at_offset with computed byte_offset.
pub struct SegmentRef<'a> {
    pub variant: usize,
    pub segment_index: usize,
    pub byte_offset: u64,
    pub data: &'a SegmentData,
}

#[kithara::test]
fn find_at_offset_returns_correct_segment() {
    let mut idx = StreamIndex::new(1, 3);
    idx.commit_segment(0, 0, make_segment_data(100, 500)); // 0..600
    idx.commit_segment(0, 1, make_segment_data(0, 400));   // 600..1000
    idx.commit_segment(0, 2, make_segment_data(0, 300));   // 1000..1300

    let seg = idx.find_at_offset(0).unwrap();
    assert_eq!(seg.segment_index, 0);
    assert_eq!(seg.byte_offset, 0);

    let seg = idx.find_at_offset(599).unwrap();
    assert_eq!(seg.segment_index, 0);

    let seg = idx.find_at_offset(600).unwrap();
    assert_eq!(seg.segment_index, 1);
    assert_eq!(seg.byte_offset, 600);

    let seg = idx.find_at_offset(1299).unwrap();
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

    let seg = idx.find_at_offset(250).unwrap();
    assert_eq!(seg.variant, 3);
    assert_eq!(seg.segment_index, 2);
    assert_eq!(seg.byte_offset, 200);
}

#[kithara::test]
fn is_range_loaded_contiguous() {
    let mut idx = StreamIndex::new(1, 3);
    idx.commit_segment(0, 0, make_segment_data(0, 100));
    idx.commit_segment(0, 1, make_segment_data(0, 100));

    assert!(idx.is_range_loaded(&(0..200)));
    assert!(idx.is_range_loaded(&(50..150)));
    assert!(!idx.is_range_loaded(&(0..300))); // seg 2 not committed
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
    // byte_map should be rebuilt: seg 0 at 0..100, seg 2 at 100..200
    assert_eq!(idx.max_end_offset(), 200);
}
```

- [ ] **Step 2: Run tests — verify FAIL**

- [ ] **Step 3: Implement find_at_offset, is_range_loaded, on_segment_invalidated**

```rust
impl StreamIndex {
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

    pub fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        self.byte_map.gaps(range).next().is_none()
    }

    pub fn on_segment_invalidated(&mut self, variant: usize, segment_index: usize) {
        if let Some(vs) = self.variants.get_mut(variant) {
            if vs.contains(segment_index) {
                vs.remove(segment_index);
                self.rebuild_byte_map_from(0); // full rebuild
            }
        }
    }
}
```

- [ ] **Step 4: Run tests — verify PASS**

- [ ] **Step 5: Commit**

`git commit -m "feat(hls): StreamIndex find_at_offset, is_range_loaded, on_segment_invalidated"`

---

### Task 5: StreamIndex — total_bytes and LayoutIndex

**Files:**
- Modify: `crates/kithara-hls/src/stream_index.rs`
- Modify: `crates/kithara-hls/src/playlist.rs` (add `segment_size` to `PlaylistAccess`)

- [ ] **Step 1: Add `segment_size` to PlaylistAccess trait**

In `playlist.rs`, add to the `PlaylistAccess` trait:

```rust
/// Size of a specific segment in bytes (from VariantSizeMap).
fn segment_size(&self, variant: usize, index: usize) -> Option<u64>;
```

And implement:

```rust
fn segment_size(&self, variant: usize, index: usize) -> Option<u64> {
    let lock = self.variants.get(variant)?;
    let state = lock.lock_sync_read();
    let size_map = state.size_map.as_ref()?;
    size_map.segment_sizes.get(index).copied()
}
```

- [ ] **Step 2: Write failing tests for total_bytes**

```rust
// In stream_index.rs tests — need a mock PlaylistAccess or test helper.
// Use the existing PlaylistState from playlist.rs for integration.

#[kithara::test]
fn total_bytes_all_committed() {
    let mut idx = StreamIndex::new(1, 3);
    idx.commit_segment(0, 0, make_segment_data(100, 500)); // 600
    idx.commit_segment(0, 1, make_segment_data(0, 400));   // 400
    idx.commit_segment(0, 2, make_segment_data(0, 300));   // 300

    // With no PlaylistState sizes, only committed count
    let playlist = make_empty_playlist(1, 3);
    assert_eq!(idx.total_bytes(&playlist), 1300);
}

#[kithara::test]
fn total_bytes_committed_plus_estimated() {
    let mut idx = StreamIndex::new(1, 5);
    idx.commit_segment(0, 0, make_segment_data(100, 500)); // 600 actual
    idx.commit_segment(0, 1, make_segment_data(0, 400));   // 400 actual
    // Segments 2-4 not committed — use estimates from PlaylistState

    let playlist = make_playlist_with_sizes(0, &[650, 420, 500, 500, 500]);
    // total = 600 (actual) + 400 (actual) + 500 + 500 + 500 (estimated)
    assert_eq!(idx.total_bytes(&playlist), 2500);
}

#[kithara::test]
fn total_bytes_after_multiple_switches_with_drm() {
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

    // Segment 5: estimated from v0 size map = 100 (encrypted)
    let playlist = make_playlist_with_variant_sizes(&[
        (0, &[100, 100, 100, 100, 100, 100]),  // v0 encrypted
        (3, &[1000, 1000, 1000, 1000, 1000, 1000]), // v3 encrypted
    ]);

    // total = 90 + 90 + 985 + 985 + 90 + 100(estimated) = 2340
    assert_eq!(idx.total_bytes(&playlist), 2340);
}
```

- [ ] **Step 3: Run tests — verify FAIL**

- [ ] **Step 4: Implement total_bytes and LayoutIndex**

```rust
impl StreamIndex {
    pub fn total_bytes(&self, playlist: &dyn PlaylistAccess) -> u64 {
        let mut total = 0u64;
        for (seg_range, &variant) in self.variant_map.iter() {
            for seg_idx in seg_range.clone() {
                total += match self.variants[variant].get(seg_idx) {
                    Some(data) => data.total_len(),
                    None => playlist.segment_size(variant, seg_idx).unwrap_or(0),
                };
            }
        }
        total
    }

    pub fn effective_total(&self, playlist: &dyn PlaylistAccess) -> u64 {
        self.max_end_offset().max(self.total_bytes(playlist))
    }
}

impl LayoutIndex for StreamIndex {
    type Item = (usize, usize);

    fn item_at_offset(&self, offset: u64) -> Option<Self::Item> {
        self.byte_map.get(&offset).copied()
    }

    fn item_range(&self, (variant, seg_idx): Self::Item) -> Option<Range<u64>> {
        // Find the byte range for this (variant, seg_idx) in byte_map
        self.byte_map.iter()
            .find(|(_, &(v, s))| v == variant && s == seg_idx)
            .map(|(range, _)| range.clone())
    }
}
```

- [ ] **Step 5: Run tests — verify PASS**

- [ ] **Step 6: Commit**

`git commit -m "feat(hls): StreamIndex total_bytes derivation and LayoutIndex impl"`

---

## Chunk 2: on_invalidated Callback

### Task 6: Add on_invalidated to StoreOptions and CachedAssets

**Files:**
- Modify: `crates/kithara-assets/src/store.rs` (StoreOptions)
- Modify: `crates/kithara-assets/src/cache.rs` (CachedAssets — fire callback on LRU displacement)

- [ ] **Step 1: Write failing test for CachedAssets invalidation callback**

In `crates/kithara-assets/src/cache.rs` tests:

```rust
#[kithara::test]
fn cache_fires_on_invalidated_when_entry_displaced() {
    let cancel = CancellationToken::new();
    let (inner, _) = make_test_store(cancel.clone());
    let invalidated = Arc::new(Mutex::new(Vec::<ResourceKey>::new()));
    let inv = Arc::clone(&invalidated);
    let on_invalidated: Arc<dyn Fn(&ResourceKey) + Send + Sync> =
        Arc::new(move |key| inv.lock_sync().push(key.clone()));

    let cache = CachedAssets::with_on_invalidated(
        Arc::new(inner),
        NonZeroUsize::new(2).unwrap(), // capacity = 2
        Some(on_invalidated),
    );

    // Open 3 resources — first should be displaced from LRU
    let _r1 = cache.open_resource(&ResourceKey::new("asset", "r1")).unwrap();
    let _r2 = cache.open_resource(&ResourceKey::new("asset", "r2")).unwrap();
    let _r3 = cache.open_resource(&ResourceKey::new("asset", "r3")).unwrap();

    let inv = invalidated.lock_sync();
    assert_eq!(inv.len(), 1);
    assert_eq!(inv[0], ResourceKey::new("asset", "r1"));
}
```

- [ ] **Step 2: Run test — verify FAIL**

- [ ] **Step 3: Implement**

In `store.rs`, add field to `StoreOptions`:

```rust
pub on_invalidated: Option<Arc<dyn Fn(&ResourceKey) + Send + Sync>>,
```

In `cache.rs`, add `on_invalidated` field to `CachedAssets` and fire on LRU displacement:

```rust
pub fn with_on_invalidated(
    inner: Arc<A>,
    capacity: NonZeroUsize,
    on_invalidated: Option<Arc<dyn Fn(&ResourceKey) + Send + Sync>>,
) -> Self { ... }

// In open_resource_with_ctx, after cache.push():
let evicted = cache.push(cache_key, CacheEntry::Resource(res.clone()));
if let Some((CacheKey::Resource(key, _), _)) = evicted {
    if let Some(cb) = &self.on_invalidated {
        cb(&key);
    }
}
```

Wire through `AssetStoreBuilder` → pass `on_invalidated` from `StoreOptions` to `CachedAssets::with_on_invalidated`.

- [ ] **Step 4: Run test — verify PASS**

- [ ] **Step 5: Run full kithara-assets tests**

Run: `cargo test -p kithara-assets`

- [ ] **Step 6: Commit**

`git commit -m "feat(assets): add on_invalidated callback for cache displacement"`

---

## Chunk 3: Migration — Downloader

### Task 7: Replace DownloadState usage in downloader.rs

**Files:**
- Modify: `crates/kithara-hls/src/downloader.rs`
- Modify: `crates/kithara-hls/src/inner.rs` (StreamIndex construction, wiring)

This is the largest task. Key changes:

1. `self.segments: Arc<Mutex<DownloadState>>` → `self.segments: Arc<RwLock<StreamIndex>>`
2. `commit_segment`: replace `push` + `fence_at` + `cascade_contiguity` → `StreamIndex::commit_segment` + `switch_variant`
3. Remove `calculate_effective_total`, `reconcile_total_bytes`
4. `refresh_variant_total_bytes` → derive total via `StreamIndex::total_bytes(&self.playlist_state)`
5. `reset_for_seek_epoch` → `StreamIndex::reset_to`

- [ ] **Step 1: Update type alias and construction**

Change `Arc<Mutex<DownloadState>>` to `Arc<RwLock<StreamIndex>>` in `HlsDownloader` and `HlsSource`.
Update `inner.rs` where the shared state is created.

- [ ] **Step 2: Build — fix all type errors in downloader.rs**

This will be iterative. For each call site:

| Old | New |
|-----|-----|
| `segments.lock_sync().push(segment)` | `segments.lock_sync_write().commit_segment(v, idx, data)` |
| `segments.lock_sync().fence_at(off, v)` | (removed — handled by switch_variant) |
| `segments.lock_sync().cascade_contiguity(v, idx)` | (removed — handled by rebuild_byte_map_from) |
| `segments.lock_sync().find_loaded_segment(v, idx)` | `segments.lock_sync_read().is_segment_loaded(v, idx)` or `.find_at_offset()` |
| `segments.lock_sync().max_end_offset()` | `segments.lock_sync_read().max_end_offset()` |
| `segments.lock_sync().is_segment_loaded(v, idx)` | `segments.lock_sync_read().is_segment_loaded(v, idx)` |

Replace `calculate_effective_total` calls with `segments.lock_sync_read().total_bytes(&self.playlist_state)`.

Remove `reconcile_total_bytes` method entirely — `StreamIndex::reconcile_segment` handles it.

In `reset_for_seek_epoch`: replace download_position/total_bytes gymnastics with `segments.lock_sync_write().reset_to(segment_index, variant)`.

- [ ] **Step 3: Build — verify compilation**

Run: `cargo check -p kithara-hls`

- [ ] **Step 4: Run existing downloader unit tests**

Run: `cargo test -p kithara-hls downloader`

Fix test helpers: update `build_pair()`, mock construction to create `StreamIndex` instead of `DownloadState`.

- [ ] **Step 5: Commit**

`git commit -m "refactor(hls): migrate downloader.rs from DownloadState to StreamIndex"`

---

### Task 8: Migrate source.rs

**Files:**
- Modify: `crates/kithara-hls/src/source.rs`

- [ ] **Step 1: Update lock calls**

All `segments.lock_sync()` → `segments.lock_sync_read()` for reads, `segments.lock_sync_write()` for mutations.

Key changes:

| Old | New |
|-----|-----|
| `segments.find_at_offset(off)` → `LoadedSegment` | `segments.find_at_offset(off)` → `SegmentRef` |
| `seg.variant`, `seg.segment_index`, `seg.byte_offset` | Same fields on `SegmentRef` |
| `seg.init_len`, `seg.media_len`, `seg.init_url`, `seg.media_url` | Via `seg.data.*` |
| `segments.fence_at(off, v)` (1 site) | `segments.switch_variant(seg_idx, v)` |
| `segments.clear()` (1 site) | `segments.reset_to(seg_idx, variant)` |
| `segments.first_segment_of_variant(v)` | `segments.variants[v].first()` + byte_offset computation |
| `segments.last()` | Use `find_at_offset(max_end_offset() - 1)` or track separately |

- [ ] **Step 2: Build and fix**

Run: `cargo check -p kithara-hls`

- [ ] **Step 3: Run source tests**

Run: `cargo test -p kithara-hls source`

- [ ] **Step 4: Commit**

`git commit -m "refactor(hls): migrate source.rs from DownloadState to StreamIndex"`

---

### Task 9: Migrate layout.rs and context.rs

**Files:**
- Modify: `crates/kithara-hls/src/layout.rs`
- Modify: `crates/kithara-hls/src/context.rs`

- [ ] **Step 1: Update layout.rs**

`first_segment_of_variant(v)` and `last_of_variant(v)` → use `StreamIndex` variant access methods.

May need to add helper methods to `StreamIndex`:
```rust
pub fn first_of_variant(&self, variant: usize) -> Option<(usize, u64, &SegmentData)>
pub fn last_of_variant(&self, variant: usize) -> Option<(usize, u64, &SegmentData)>
```

- [ ] **Step 2: Update context.rs**

`find_at_offset` and `last()` calls → use `StreamIndex` API.

- [ ] **Step 3: Build and fix**

Run: `cargo check -p kithara-hls`

- [ ] **Step 4: Run all kithara-hls tests**

Run: `cargo test -p kithara-hls`

- [ ] **Step 5: Commit**

`git commit -m "refactor(hls): migrate layout.rs and context.rs to StreamIndex"`

---

## Chunk 4: Cleanup and Validation

### Task 10: Remove DownloadState and dead code

**Files:**
- Delete or gut: `crates/kithara-hls/src/download_state.rs`
- Modify: `crates/kithara-hls/src/lib.rs` (remove `mod download_state`)
- Modify: `crates/kithara-hls/src/downloader.rs` (remove dead functions)

- [ ] **Step 1: Remove download_state.rs**

Delete the file. Remove `mod download_state` from `lib.rs`. Remove all `use` of `DownloadState`, `LoadedSegment`, `DownloadProgress`.

- [ ] **Step 2: Remove dead downloader functions**

Remove from `downloader.rs`:
- `calculate_effective_total()`
- `reconcile_total_bytes()`
- `resolve_byte_offset()` (if fully replaced)
- Any helper functions that only served the old DownloadState

- [ ] **Step 3: Build workspace**

Run: `cargo check --workspace`

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 5: Run fmt**

Run: `cargo fmt --all`

- [ ] **Step 6: Commit**

`git commit -m "refactor(hls): remove DownloadState and dead total_bytes formula code"`

---

### Task 11: Wire on_invalidated callback in HLS

**Files:**
- Modify: `crates/kithara-hls/src/inner.rs` or wherever AssetStore is built for HLS
- Modify: `crates/kithara-hls/src/config.rs` (if needed)

- [ ] **Step 1: Wire callback from StoreOptions to StreamIndex**

Where `AssetStore` is built (in HLS setup), add:

```rust
store_options.on_invalidated = Some({
    let idx = Arc::clone(&stream_index);
    let bus = event_bus.clone();
    Arc::new(move |key: &ResourceKey| {
        if let Some((variant, seg_idx)) = parse_segment_key(key) {
            idx.lock_sync_write().on_segment_invalidated(variant, seg_idx);
            bus.publish(HlsEvent::SegmentInvalidated { variant, segment_index: seg_idx });
        }
    })
});
```

- [ ] **Step 2: Add HlsEvent::SegmentInvalidated variant**

In `kithara-events`:
```rust
pub enum HlsEvent {
    // ... existing variants ...
    SegmentInvalidated { variant: usize, segment_index: usize },
}
```

- [ ] **Step 3: Add parse_segment_key helper**

Parse variant and segment_index from `ResourceKey` (based on existing naming convention for HLS resources).

- [ ] **Step 4: Build and test**

Run: `cargo test -p kithara-hls`

- [ ] **Step 5: Commit**

`git commit -m "feat(hls): wire on_invalidated callback for cache-displaced segments"`

---

### Task 12: Integration Validation

**Files:**
- Test: `tests/tests/kithara_hls/drm_stream_integrity.rs`

- [ ] **Step 1: Run drm_stream_byte_integrity — all 12 cases**

Run: `cargo test -p kithara-integration-tests drm_stream_byte_integrity -- --nocapture`

Expected: 12/12 PASS (including DRM auto cases that previously failed with 484K discrepancy)

- [ ] **Step 2: Run full kithara-hls test suite**

Run: `cargo test -p kithara-hls`

- [ ] **Step 3: Run workspace tests**

Run: `cargo test --workspace`

- [ ] **Step 4: Run lint**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --all --check`

- [ ] **Step 5: Run style check**

Run: `ast-grep scan --config sgconfig.yml --report-style short --filter '^(style.no-tests-in-lib-or-mod-rs|rust.no-thin-async-wrapper)$'`

- [ ] **Step 6: Commit any test fixes**

`git commit -m "test(hls): validate StreamIndex refactor — drm_stream_byte_integrity 12/12"`

---

## Summary

| Task | What | Files | Risk |
|------|------|-------|------|
| 1 | SegmentData + VariantSegments | stream_index.rs (new) | Low — additive |
| 2 | StreamIndex core — variant_map | stream_index.rs | Low — additive |
| 3 | commit/reconcile + byte_map | stream_index.rs | Medium — core logic |
| 4 | find_at_offset + invalidation | stream_index.rs | Medium — core logic |
| 5 | total_bytes + LayoutIndex | stream_index.rs, playlist.rs | Medium — new derivation |
| 6 | on_invalidated callback | store.rs, cache.rs | Low — additive |
| 7 | Migrate downloader.rs | downloader.rs, inner.rs | **High** — largest change |
| 8 | Migrate source.rs | source.rs | **High** — read path |
| 9 | Migrate layout/context | layout.rs, context.rs | Low — small |
| 10 | Remove dead code | download_state.rs, downloader.rs | Medium — cleanup |
| 11 | Wire invalidation callback | inner.rs, events | Low — wiring |
| 12 | Integration validation | tests | Low — verification |
