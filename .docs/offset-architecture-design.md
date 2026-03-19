# Offset Architecture Design — v2

## Root Cause (from log evidence)

The **single root cause** is that `set_seek_epoch()` unconditionally destroys the
committed byte layout (clears DownloadState), but `align_decoder_with_seek_anchor()`
does NOT recreate the decoder for same-variant seeks. This creates a mismatch:
Symphonia's internal tables reference a layout that no longer exists.

**Log evidence:**
```
seek anchor alignment: codec_changed=false variant_changed=false stale_base_offset=false
seek anchor path: decoder seek to segment start failed
  err=SeekFailed("seek past EOF: new_pos=1162117088 len=1860569")
seek anchor path failed, falling back to direct seek
wait_range: range_start=0  ← reader at position 0, not at seek target!
```

**Secondary issue** (DRM only): `request_on_demand_segment()` uses metadata
`find_segment_at_offset(committed_position)`. After PKCS7 padding removal,
committed positions diverge from metadata → wrong segment → spin-loop.

## Key Insight

> "The real invariant is not 'same variant' — it is 'same committed byte layout'."
> — Codex review

If we **preserve the committed byte layout** for same-variant seeks, Symphonia's
internal tables remain valid. No decoder recreation needed. No stale state.

The fix: **classify seeks before flush**. Only clear DownloadState when the
byte layout must change (variant switch). For same-variant seeks, keep segments
in place and just move the reader position.

## Design

### Overview

```
seek_time_anchor(position):
  anchor = resolve_seek_anchor(position)  // side-effect-free
  layout = classify_seek(anchor)          // Preserve or Reset
  apply_seek_plan(anchor, layout)         // clear only for Reset
```

All changes are internal to `kithara-hls`. No `Source` trait modifications.

### Change 1: Non-destructive `set_seek_epoch()` (Problem A root fix)

**File:** `crates/kithara-hls/src/source.rs`

**Current:** `set_seek_epoch()` unconditionally clears segments and resets
download_position to 0.

**New:** Only perform non-destructive operations. Move layout-specific reset
into `seek_time_anchor()` via the seek plan.

```rust
fn set_seek_epoch(&mut self, _seek_epoch: u64) {
    self.shared.timeline.set_eof(false);
    self.shared.had_midstream_switch.store(false, Ordering::Release);
    // Drain stale requests — new ones will be pushed by seek_time_anchor
    while self.shared.segment_requests.pop().is_some() {}
    // DO NOT clear segments or reset download_position
    self.shared.reader_advanced.notify_one();
    self.shared.condvar.notify_all();
}
```

### Change 2: Seek classification in `seek_time_anchor()`

**File:** `crates/kithara-hls/src/source.rs`

Add internal enum and helpers:

```rust
enum SeekLayout {
    /// Same variant — keep DownloadState, byte layout unchanged
    Preserve,
    /// Different variant — clear DownloadState, rebuild layout
    Reset,
}
```

**`current_layout_variant()`**: Returns the variant of the committed byte layout.
Prefer committed segment at current byte_position → variant fence → ABR hint.

```rust
fn current_layout_variant(&self) -> Option<usize> {
    let pos = self.shared.timeline.byte_position();
    let segments = self.shared.segments.lock_sync();
    if let Some(seg) = segments.find_at_offset(pos) {
        return Some(seg.variant);
    }
    drop(segments);
    // variant_fence is set from first read_at after format change
    if let Some(fence) = self.variant_fence {
        return Some(fence);
    }
    // ABR hint is last resort
    Some(self.shared.abr_variant_index.load(Ordering::Acquire))
}
```

**`classify_seek()`**: Compare target variant with current layout variant.

```rust
fn classify_seek(&self, anchor: &SourceSeekAnchor) -> SeekLayout {
    let target_variant = anchor.variant_index.unwrap_or(0);
    match self.current_layout_variant() {
        Some(current) if current == target_variant => SeekLayout::Preserve,
        _ => SeekLayout::Reset,
    }
}
```

### Change 3: Unified `seek_time_anchor()` with seek plan

**File:** `crates/kithara-hls/src/source.rs`

```rust
fn seek_time_anchor(&mut self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>, HlsError> {
    let anchor = self.resolve_seek_anchor(position).map_err(StreamError::Source)?;
    let layout = self.classify_seek(&anchor);
    self.apply_seek_plan(&anchor, layout);
    Ok(Some(anchor))
}

fn apply_seek_plan(&mut self, anchor: &SourceSeekAnchor, layout: SeekLayout) {
    let variant = anchor.variant_index.unwrap_or(0);
    let segment_index = anchor.segment_index.unwrap_or(0) as usize;
    let seek_epoch = self.shared.timeline.seek_epoch();

    // Always: drain requests and queue target
    while self.shared.segment_requests.pop().is_some() {}
    self.shared.segment_requests.push(SegmentRequest {
        segment_index,
        variant,
        seek_epoch,
    });

    match layout {
        SeekLayout::Preserve => {
            // Keep segments — byte layout valid, Symphonia tables remain correct
            // download_position stays as committed watermark
        }
        SeekLayout::Reset => {
            // Clear segments — layout changes, decoder will be recreated by
            // audio pipeline due to codec_changed or variant_changed
            let mut segments = self.shared.segments.lock_sync();
            segments.clear();
            drop(segments);
            self.shared.timeline.set_download_position(0);
            self.shared.current_segment_index.store(0, Ordering::Release);
        }
    }

    // Always update reader position to target
    self.shared.timeline.set_byte_position(anchor.byte_offset);
    self.shared.current_segment_index
        .store(anchor.segment_index.unwrap_or(0), Ordering::Relaxed);
    self.shared.reader_advanced.notify_one();
    self.shared.condvar.notify_all();
}
```

### Change 4: Same-variant-aware `reset_for_seek_epoch()` in downloader

**File:** `crates/kithara-hls/src/downloader.rs`

**Current:** Always sets `download_position` from metadata offset.

**New:** For same-variant seeks, keep download_position as the committed watermark.
For variant changes, keep current behavior.

```rust
fn reset_for_seek_epoch(&mut self, seek_epoch: SeekEpoch, variant: usize, segment_index: usize) {
    let previous_variant = self.last_committed_variant;
    let is_same_variant = previous_variant.is_some_and(|prev| prev == variant);

    self.active_seek_epoch = seek_epoch;
    self.shared.timeline.set_eof(false);
    self.shared.had_midstream_switch.store(false, Ordering::Release);
    self.current_segment_index = segment_index;
    self.gap_scan_start_segment = segment_index;

    self.force_init_for_seek = previous_variant
        .and_then(|index| self.playlist_state.variant_codec(index))
        .zip(self.playlist_state.variant_codec(variant))
        .is_some_and(|(from, to)| from != to);

    self.last_committed_variant = Some(variant);

    let variant_total = self.playlist_state.total_variant_size(variant);
    self.shared.timeline.set_total_bytes(
        variant_total.or_else(|| self.shared.timeline.total_bytes())
    );

    if is_same_variant {
        // Same variant: keep download_position at committed watermark.
        // Segments are still in DownloadState — downloader will check
        // segment_loaded_for_demand() and skip already-loaded segments.
        let watermark = self.shared.timeline.download_position();
        let target_offset = self.playlist_state
            .segment_byte_offset(variant, segment_index)
            .unwrap_or(0);
        // Ensure download_position covers at least the target
        self.shared.timeline.set_download_position(
            watermark.max(target_offset)
        );
    } else {
        // Different variant: full reset
        let download_position = self.playlist_state
            .segment_byte_offset(variant, segment_index)
            .unwrap_or(0);
        self.shared.timeline.set_download_position(download_position);
    }

    let current_variant = self.abr.get_current_variant_index();
    if current_variant != variant {
        self.abr.apply(
            &AbrDecision {
                target_variant_index: variant,
                reason: AbrReason::ManualOverride,
                changed: true,
            },
            Instant::now(),
        );
    }
}
```

### Change 5: Committed-first lookup in `request_on_demand_segment()` (Problem B)

**File:** `crates/kithara-hls/src/source_wait_range.rs`

**Current:** Uses metadata `find_segment_at_offset(range_start)` directly.

**New:** Check committed DownloadState first:

```rust
fn request_on_demand_segment(&self, range_start: u64, seek_epoch: u64, ...) {
    let current_variant = self.resolve_current_variant();

    // Step 1: Check committed data — authoritative for DRM-reconciled offsets
    {
        let segments = self.shared.segments.lock_sync();

        // If range_start falls within a committed segment
        if let Some(seg) = segments.find_at_offset(range_start) {
            let idx = seg.segment_index;
            drop(segments);
            self.push_segment_request(current_variant, idx, seek_epoch);
            return Ok(true);
        }

        // If range_start is past all committed data → request next segment
        if let Some(last) = segments.last_of_variant(current_variant) {
            if range_start >= last.end_offset() {
                let next_idx = last.segment_index + 1;
                drop(segments);
                if let Some(num) = self.playlist_state.num_segments(current_variant) {
                    if next_idx < num {
                        self.push_segment_request(current_variant, next_idx, seek_epoch);
                        return Ok(true);
                    }
                }
            }
        }
    }

    // Step 2: Fall back to metadata lookup
    // Safe for empty DownloadState (after Reset seek): reader position was
    // also set from metadata in seek_time_anchor, so same-space lookup.
    if let Some(segment_index) = self.playlist_state
        .find_segment_at_offset(current_variant, range_start)
    {
        self.push_segment_request(current_variant, segment_index, seek_epoch);
        return Ok(true);
    }

    // ... existing fallback_segment_index_for_offset + metadata miss logic ...
}
```

Add helpers to `DownloadState`:

```rust
impl DownloadState {
    /// Find the last committed segment for a specific variant.
    pub fn last_of_variant(&self, variant: usize) -> Option<&LoadedSegment> {
        self.segments.values().rev()
            .find(|s| s.variant == variant)
    }
}
```

## Implementation Order & Workflow

### Success criteria
- `cargo test` — **ALL tests green** (unit, integration, heavy)

### Workflow per step (TDD)
Each step follows a strict RED → GREEN → REVIEW gate:
1. **RED**: Write a failing test that captures the desired behavior
2. **`cargo test`** — confirm test FAILS (red)
3. **Send RED test to Gemini** — validate test correctness and coverage
4. **GREEN**: Write minimal code to make the test pass
5. **`cargo test -p <affected-crate>`** — confirm test PASSES (green)
6. **`cargo test --workspace`** — no regressions
7. **Send diff (test + impl) to Codex** — code review
8. **Fix** any issues found by reviewers
9. **Move to next step** only after gate passes

### Steps

#### Step 1: Non-destructive `set_seek_epoch` + seek classification (Changes 1-3)
- Files: `crates/kithara-hls/src/source.rs`
- **RED**: Test that same-variant seek preserves DownloadState segments
- **RED**: Test that variant-change seek clears DownloadState segments
- **GREEN**: Add `SeekLayout` enum, `current_layout_variant()`, `classify_seek()`
- **GREEN**: Make `set_seek_epoch()` non-destructive
- **GREEN**: Update `seek_time_anchor()` to classify and apply Preserve/Reset plan
- Gate: Gemini review (RED) → Codex review (GREEN) → all HLS tests pass

#### Step 2: Reorder `complete_seek` in audio pipeline
- File: `crates/kithara-audio/src/pipeline/source.rs`
- **RED**: Test that `seek_time_anchor` completes before `complete_seek` unblocks downloader
- **GREEN**: Move `complete_seek(epoch)` after `seek_time_anchor()`
- Gate: Gemini review → Codex review → audio tests pass

#### Step 3: Same-variant-aware `reset_for_seek_epoch` in downloader (Change 4)
- File: `crates/kithara-hls/src/downloader.rs`
- **RED**: Test that same-variant seek keeps download_position watermark
- **RED**: Test that variant-change seek resets download_position
- **GREEN**: Update `reset_for_seek_epoch()` with same-variant awareness
- Gate: Gemini review → Codex review → downloader tests pass

#### Step 4: Committed-first lookup in `request_on_demand_segment` (Change 5)
- Files: `crates/kithara-hls/src/source_wait_range.rs`, `crates/kithara-hls/src/download_state.rs`
- **RED**: Test that committed offset (post-DRM) resolves correct segment
- **RED**: Test that past-committed offset requests next segment
- **GREEN**: Add `last_of_variant()` to DownloadState
- **GREEN**: Committed-first lookup with metadata fallback
- Gate: Gemini review → Codex review → HLS tests pass

#### Step 5: Full verification
- `cargo test --workspace` — all green
- `cargo clippy --workspace -- -D warnings`
- `cargo fmt --all --check`
- Codex final review of complete changeset

## What This Changes

| Component | Before | After |
|-----------|--------|-------|
| `set_seek_epoch()` | Clears segments, resets download_position | Only drains requests, resets flags |
| `seek_time_anchor()` | Resolves anchor, queues request | Resolves anchor, classifies seek, applies plan |
| `reset_for_seek_epoch()` (downloader) | Always resets download_position from metadata | Same-variant: keeps watermark |
| `request_on_demand_segment()` | Metadata-only lookup | Committed-first, metadata fallback |

## What This Doesn't Change

- `align_decoder_with_seek_anchor()` — same logic (codec_changed || stale_base_offset)
- `decoder_recreate_offset()` — same priority chain
- `resolve_byte_offset()` — still uses metadata for non-switch
- `reconcile_segment_size()` — still updates metadata after DRM decrypt
- ABR switch path — variant/codec change triggers recreation as before
- `byte_offset_for_segment()` — still prefers metadata (correct for Preserve; for Reset
  the DownloadState is empty so metadata is the only source)

## Why This Works

**Same-variant seek (Preserve):**
1. `set_seek_epoch()` drains requests, resets flags — but keeps segments
2. `seek_time_anchor()` resolves anchor at metadata offset for target segment
3. `classify_seek()` → `Preserve` (current variant == target variant)
4. `apply_seek_plan()`: keeps segments, moves `byte_position` to target offset
5. Audio pipeline: `align_decoder_with_seek_anchor()` → `!codec_changed && !stale_base_offset` → no recreation
6. `apply_time_anchor_seek()` → `decoder_seek_safe(anchor.segment_start)` → Symphonia uses its VALID internal tables → **seek succeeds**
7. If target segment not yet loaded: `wait_range` → `request_on_demand_segment()` → committed lookup finds segment or requests next one

**Variant-change seek (Reset):**
1. `set_seek_epoch()` same as above
2. `seek_time_anchor()` → `classify_seek()` → `Reset`
3. `apply_seek_plan()`: clears segments, resets download_position
4. Audio pipeline: `align_decoder_with_seek_anchor()` → `codec_changed` or `variant_changed` → decoder recreated
5. New decoder probes init segment from target offset → fresh tables → seek succeeds

**DRM after Preserve seek:**
- Segments stay in DownloadState with committed (post-decrypt) offsets
- Reader position moves through committed offsets
- `request_on_demand_segment()` checks committed data first → correct segment index
- No cross-space lookup

## Edge Cases

1. **Segment eviction (LRU cache):** Segment entries stay in DownloadState but
   resource bytes may be evicted. Existing `read_at()` and `segment_loaded_for_demand()`
   already handle this (re-request on eviction miss). No change needed.

2. **ABR hint changed to same-codec different variant:** `classify_seek()` uses
   `current_layout_variant()` which prefers committed variant, not ABR hint.
   Seek stays on committed variant. ABR switch happens normally via
   `had_midstream_switch` path.

3. **`base_offset > 0` from prior ABR switch:** `align_decoder_with_seek_anchor()`
   still checks `stale_base_offset` → triggers recreation. This path unchanged.

4. **Empty DownloadState (fresh start, no segments loaded yet):**
   `current_layout_variant()` returns ABR hint → if target matches, Preserve.
   But with empty DownloadState, Preserve is effectively same as Reset
   (nothing to clear). Metadata fallback in `request_on_demand_segment()` handles it.

5. **download_position watermark after Preserve seek backward:**
   download_position stays at the high watermark. Downloader's
   `segment_loaded_for_demand()` skips already-loaded segments. Only missing
   segments are downloaded.

## Risks

1. **Stale segments after Preserve:** If reconcile changed metadata offsets for
   earlier segments but DownloadState has old committed offsets, there's still a
   gap. Mitigated: committed-first lookup (Change 5) uses the actual committed
   offsets, not metadata.

2. **Memory from preserved segments:** Preserved segments stay in DownloadState
   across seeks. For long sessions with many seeks, this could grow.
   Mitigated: LRU eviction still frees the actual byte resources. DownloadState
   entries are lightweight (~100 bytes each).

3. **Regression in non-HLS paths:** File sources return `seek_time_anchor() → None`
   and `set_seek_epoch()` is a no-op. No impact.
