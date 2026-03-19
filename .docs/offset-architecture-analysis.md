# Byte Offset Architecture Analysis

## Problem Statement

HLS + DRM (AES-128-CBC) path has order-sensitive test failures.
Root cause: two independent offset spaces that diverge after decryption.

## Two Offset Spaces

### Metadata Space (PlaylistState)
- Built from HEAD requests (pre-decrypt/encrypted sizes)
- `VariantSizeMap.offsets[]` — cumulative byte offsets
- `VariantSizeMap.segment_sizes[]` — per-segment total sizes
- Updated by `reconcile_segment_size()` AFTER each segment downloads

### Committed Space (DownloadState)
- Built from actual downloads (post-decrypt sizes)
- `LoadedSegment.byte_offset` — where segment was placed
- `BTreeMap<u64, LoadedSegment>` — ordered by committed byte_offset

## Byte Offset Usage Table

### A. PlaylistState — METADATA offsets

| File:Line | Function | Source | Type |
|-----------|----------|--------|------|
| playlist.rs:300-305 | `segment_byte_offset()` | `size_map.offsets[index]` | metadata |
| playlist.rs:311-329 | `find_segment_at_offset()` | binary search on metadata offsets | metadata |
| playlist.rs:158-182 | `reconcile_segment_size()` | updates sizes/offsets with actual | metadata→bridge |
| playlist.rs:290-293 | `total_variant_size()` | `size_map.total` | metadata |

### B. DownloadState — COMMITTED offsets

| File:Line | Function | Source | Type |
|-----------|----------|--------|------|
| download_state.rs:27 | `LoadedSegment.byte_offset` | placed at commit time | committed |
| download_state.rs:150-156 | `find_at_offset()` | BTreeMap lookup | committed |
| download_state.rs:167-175 | `find_loaded_segment()` | by (variant, segment_index) | committed |
| download_state.rs:208-213 | `max_end_offset()` | highest committed end | committed |

### C. HlsDownloader — MIXED

| File:Line | Function | Source | Type | Split-brain? |
|-----------|----------|--------|------|:---:|
| downloader.rs:200-203 | `reset_for_seek_epoch()` | `segment_byte_offset()` → download_position | metadata | ⚠️ |
| downloader.rs:249-264 | `loaded_segment_offset_mismatch()` | committed vs metadata comparison | mixed | ⚠️ |
| downloader.rs:435 | `populate_cached_segments()` | cumulative from disk sizes | committed | ✓ |
| downloader.rs:498-508 | `resolve_byte_offset()` | metadata OR download_position | **MIXED** | 🔴 |
| downloader.rs:516-519 | `reconcile_total_bytes()` | calls reconcile_segment_size | bridge | ✓ |
| downloader.rs:564 | `commit_segment()` | calls resolve_byte_offset | mixed | 🔴 |
| downloader.rs:723-761 | `calculate_effective_total()` | segment_byte_offset for switch | metadata | ⚠️ |

### D. HlsSource — MIXED

| File:Line | Function | Source | Type | Split-brain? |
|-----------|----------|--------|------|:---:|
| source.rs:161-170 | `byte_offset_for_segment()` | metadata first, committed fallback | **MIXED** | 🔴 |
| source.rs:193-200 | `resolve_seek_anchor()` | via byte_offset_for_segment | mixed | 🔴 |
| source.rs:224 | `read_from_entry()` | committed seg.byte_offset | committed | ✓ |
| source.rs:410-411 | `read_at()` | committed find_at_offset | committed | ✓ |
| source.rs:505-540 | `format_change_segment_range()` | committed first, metadata fallback | mixed | ⚠️ |
| source.rs:596 | `seek_time_anchor()` | from resolve_seek_anchor | mixed | 🔴 |

### E. Source wait_range — MIXED

| File:Line | Function | Source | Type | Split-brain? |
|-----------|----------|--------|------|:---:|
| source_wait_range.rs:184 | `request_on_demand_segment()` | metadata `find_segment_at_offset` on committed position | **MIXED** | 🔴 |
| source_wait_range.rs:86-88 | `build_wait_range_context()` | committed `max_end_offset` + metadata `total_bytes` | mixed | ⚠️ |

### F. Timeline — sync point

| Value | Set by | Space |
|-------|--------|-------|
| `byte_position` | `read_at` (committed) / `seek_time_anchor` (mixed) | mixed |
| `download_position` | `commit_segment` (mixed) / `reset_for_seek_epoch` (metadata) | mixed |
| `total_bytes` | metadata `total_variant_size()`, adjusted by `reconcile` | metadata→bridge |

## Critical Split-Brain Points (4 places)

### 1. `request_on_demand_segment()` — metadata lookup on committed position
Reader position is in committed space. `find_segment_at_offset()` uses metadata.
After DRM decrypt, committed segment ends earlier than metadata says.
Result: lookup returns already-loaded segment instead of next segment → stall.

### 2. `byte_offset_for_segment()` — prefers metadata over committed
Returns metadata offset even when committed offset differs (post-reconcile of earlier segments).
Used by `resolve_seek_anchor()` → seek lands at wrong position.

### 3. `resolve_byte_offset()` — metadata for non-switch, download_position for switch
For non-switch sequential: metadata offset used for placement. OK if reconcile keeps up.
But if segment N+1 is placed at metadata offset BEFORE segment N is reconciled, placement is wrong.

### 4. `reset_for_seek_epoch()` — metadata offset as download_position
Sets download_position from metadata. If metadata has partial reconciliation,
this position may not match where the seek target will actually be committed.

## Gemini Analysis Summary

Gemini identified three approaches:
1. **Synchronized Re-basing**: When reconcile changes offsets, update DownloadState entries too
2. **Segment-Index Centric**: Key everything by (variant, index), not by byte offset
3. **Virtual-to-Metadata Mapping**: Explicit translation layer with variant_base_offset

Recommended: Combination of #1 (for DRM) + variant_base_offset (for ABR).

## Key Insight

For **sequential** downloads, the split-brain is harmless because `reconcile` runs in
`commit_segment` before the next segment is looked up.

The split-brain manifests when:
- Reader position (committed) is used for metadata lookup (wait_range on-demand)
- Seek anchor uses metadata offset that hasn't been fully reconciled
- Segments are re-downloaded after eviction at shifted metadata offsets
