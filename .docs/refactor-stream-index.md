# Refactor: StreamIndex — unified segment layout with per-variant storage

**Created:** 2026-03-15
**Status:** Design approved, ready for implementation

## Problem

`DownloadState` stores segments from ALL variants in one `BTreeMap<u64, LoadedSegment>`.
`total_bytes` is computed by a formula (`total - switch_meta + switch_byte`) that mixes
encrypted metadata sizes with decrypted byte positions. After 2+ ABR switches with DRM,
`total_bytes` diverges by hundreds of KB (484K observed).

Data is scattered across 3 unsynchronized structures:
- `VariantSizeMap` (encrypted metadata, reconciled per-segment)
- `DownloadState` (actual committed segments)
- `Timeline.total_bytes` (formula-derived + delta-patched)

## Solution

Replace `DownloadState` with `StreamIndex`: per-variant segment storage +
two `RangeMap`s for stream composition. `total_bytes` is derived (committed actual +
estimated remaining), not formula-computed.

## Architecture

```
StreamIndex (behind Arc<RwLock<StreamIndex>>)
├── variants: Vec<VariantSegments>      // per-variant actual data
│   └── segments: BTreeMap<usize, SegmentData>  // keyed by segment_index
├── variant_map: RangeMap<usize, usize> // segment_index ranges → variant
│   // e.g., 0..3 → 0, 3..6 → 3, 6..37 → 0
├── byte_map: RangeMap<u64, (usize, usize)>  // byte ranges → (variant, seg_idx)
│   // only committed segments, O(log n) lookup
└── num_segments: usize
```

### Data Structures

```rust
/// Actual data for a committed segment (post-download, post-DRM-decrypt).
pub struct SegmentData {
    pub init_len: u64,
    pub media_len: u64,
    pub init_url: Option<Url>,
    pub media_url: Url,
}

/// All committed segments for one variant.
pub struct VariantSegments {
    segments: BTreeMap<usize, SegmentData>,
}

/// Single source of truth. Behind Arc<RwLock<>>.
pub struct StreamIndex {
    variants: Vec<VariantSegments>,
    variant_map: RangeMap<usize, usize>,
    byte_map: RangeMap<u64, (usize, usize)>,
    num_segments: usize,
}
```

### Mutations

```rust
impl StreamIndex {
    /// Commit a downloaded segment.
    fn commit_segment(&mut self, variant: usize, segment_index: usize, data: SegmentData);

    /// DRM reconciliation: actual size differs from HEAD estimate.
    fn reconcile_segment(&mut self, variant: usize, segment_index: usize, new_data: SegmentData);

    /// ABR switch at segment_index to new variant.
    fn switch_variant(&mut self, segment_index: usize, variant: usize);

    /// Reset seek: replace layout for target variant at segment.
    fn reset_to(&mut self, segment_index: usize, variant: usize);

    /// Cache invalidation callback: single segment removed.
    fn on_segment_invalidated(&mut self, variant: usize, segment_index: usize);
}
```

### Read Interface

```rust
impl StreamIndex {
    /// Core read path. O(log n) via byte_map.
    fn find_at_offset(&self, offset: u64) -> Option<SegmentRef<'_>>;

    /// Watermark: highest committed byte.
    fn max_end_offset(&self) -> u64;

    /// Derived: committed actual + estimated remaining.
    fn total_bytes(&self, size_maps: &PlaylistState) -> u64;

    /// EOF check: max(committed, total_bytes estimate).
    fn effective_total(&self, size_maps: &PlaylistState) -> u64;

    /// Is byte range fully covered by committed segments?
    fn is_range_loaded(&self, range: &Range<u64>) -> bool;

    /// Is segment committed?
    fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool;

    /// Which variant is at this segment_index (committed or expected)?
    fn variant_at(&self, segment_index: usize) -> Option<usize>;
}
```

### total_bytes Derivation

```rust
fn total_bytes(&self, size_maps: &PlaylistState) -> u64 {
    let mut total = 0u64;
    for (seg_range, &variant) in self.variant_map.iter() {
        for seg_idx in seg_range.clone() {
            total += match self.variants[variant].get(seg_idx) {
                Some(data) => data.total_len(),             // committed: actual
                None => size_maps.segment_size(variant, seg_idx).unwrap_or(0), // estimated
            };
        }
    }
    total
}
```

No formulas. No delta patching. Committed = actual decrypted. Uncommitted = HEAD estimate.
Monotonically converges as segments download. DRM overestimate is safe (actual < estimated).

### Eviction: on_invalidated Callback

CachedAssets is the actual storage in ephemeral mode. When LRU displaces a resource,
the data is lost. Callback notifies StreamIndex.

```rust
// StoreOptions (kithara-assets)
pub struct StoreOptions {
    // ... existing fields ...
    pub on_invalidated: Option<Arc<dyn Fn(&ResourceKey) + Send + Sync>>,
}

// CachedAssets: capture displaced entry
let evicted = cache.push(cache_key, CacheEntry::Resource(res.clone()));
if let Some((CacheKey::Resource(key, _), _)) = evicted {
    if let Some(cb) = &self.on_invalidated {
        cb(&key);
    }
}

// HLS wiring
StoreOptions {
    on_invalidated: Some(Arc::new(move |key| {
        if let Some((variant, seg_idx)) = parse_segment_key(key) {
            idx.lock_sync_write().on_segment_invalidated(variant, seg_idx);
            bus.publish(HlsEvent::SegmentInvalidated { variant, segment_index: seg_idx });
        }
    })),
    ..
}
```

Unconditional — fires in both disk and ephemeral modes.

### Migration: Old → New API

| source.rs calls | Was (DownloadState) | Now (StreamIndex) |
|---|---|---|
| `find_at_offset(offset)` | BTreeMap range query | `byte_map.get_key_value()` |
| `max_end_offset()` | BTreeMap last entry | `byte_map.iter().last()` |
| `is_range_loaded(range)` | `loaded_ranges: RangeSet` | `byte_map.gaps()` |
| `is_segment_loaded(v, s)` | `loaded_keys: HashSet` | `variants[v].contains(s)` |
| `first_segment_of_variant(v)` | Linear scan entries | `variants[v].first()` |
| `fence_at()` | Remove entries | **Removed** (visibility via layout) |
| `clear()` | Wipe all | `reset_to()` |

Lock: `Arc<Mutex<DownloadState>>` → `Arc<RwLock<StreamIndex>>`
- source.rs reads: `lock_sync_read()`
- downloader writes: `lock_sync_write()`

LayoutIndex trait reimplemented on StreamIndex.

## What Gets Removed

- `calculate_effective_total()` formula in downloader.rs
- `reconcile_total_bytes()` delta patching in downloader.rs
- `fence_at()` in DownloadState
- `download_position` as input for total_bytes
- `cascade_contiguity()` (replaced by `rebuild_byte_map_from`)
- `relocate_segment()` (replaced by `reconcile_segment`)
- `loaded_keys: HashSet` (replaced by `variants[v].contains()`)
- `loaded_ranges: RangeSet` (replaced by `byte_map.gaps()`)

## What Stays

- `VariantSizeMap` — for estimated remaining of uncommitted segments
- `LayoutIndex` trait — reimplemented on StreamIndex
- `LoadedSegment` fields — moved into `SegmentData` (minus variant, byte_offset)

## Decision Log

| # | Decision | Alternatives | Rationale |
|---|----------|-------------|-----------|
| 1 | Per-variant key: `BTreeMap<usize, SegmentData>` | byte_offset key, `Vec<Option>` | Natural HLS key, byte_offset computed by compositor |
| 2 | `RangeMap<usize, usize>` (variant_map) | Separate StreamLayout struct | LayoutRegion with 1 field redundant, RangeMap in deps |
| 3 | `RangeMap<u64, (usize, usize)>` (byte_map) | No cache O(S), BTreeMap | O(log n) find_at_offset, rangemap already in deps |
| 4 | total_bytes: per-region committed + estimated | Committed only, formula | Upfront estimate for Symphonia, converges to actual |
| 5 | `Arc<RwLock<StreamIndex>>` | `Arc<Mutex>`, separate locks | RwLock in kithara-platform, multiple readers |
| 6 | ABR switch: append-only regions | fence_at destructive | Data preserved, visibility via layout |
| 7 | Reset seek: replace layout single-region | Clear all, keep data | Variant data preserved, no empty state |
| 8 | `on_invalidated` in StoreOptions | EvictAssets callback, pull-based | CachedAssets = storage in ephemeral, unconditional |
| 9 | PlaylistState as argument to total_bytes() | Arc inside, per-region cache | Simplicity, no coupling |

## Testing

### Unit tests (StreamIndex)
- commit_single_variant
- commit_with_abr_switch
- total_bytes_committed_plus_estimated
- total_bytes_after_drm_reconciliation
- find_at_offset_cross_variant
- switch_variant_truncates_tail
- reset_to_replaces_layout
- on_segment_invalidated / on_segment_invalidated_unknown
- multiple_switches_total_bytes_consistent (v0→v3→v0 with DRM)

### Integration (existing)
- `drm_stream_byte_integrity` — 12 cases GREEN

### Regression guard
- New case: DRM auto with multiple switches (the 484K scenario)
