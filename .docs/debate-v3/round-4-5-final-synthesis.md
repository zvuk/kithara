# Rounds 4-5: Final Synthesis — v3 Design

## Unanimous Verdict: 5 Surgical Fixes, NOT a Rewrite

All three reviewers (Codex, Gemini, Claude) across 5 rounds conclude:
- The actor pipeline (v1/v2) was solving the wrong problem
- Symphonia's IsoMp4Reader works correctly for fMP4
- The root cause is 5 distinct lifecycle/logic bugs in kithara
- Total fix: ~2-3 days, ~50 lines changed

## Codex Round 4: Fix Trace Validated

Complete execution trace for the fix (variant 0 AAC 66k → variant 1 AAC 270k, seek 90s):

1. `align_decoder_with_seek_anchor()`: codec_changed=false, **variant_changed=true**, stale_base_offset=false
2. Guard: `!false && !true && !false = false` → falls through to recreation
3. `format_change_segment_range()` finds first init of variant 1 at offset Bsw
4. `recreate_decoder(variant_1_info, Bsw)` → new OffsetReader(Bsw) → new IsoMp4Reader
5. New reader parses variant 1's moov → correct moof seek index
6. `decoder.seek(90s)` uses correct variant 1 positions → success

**Validated:** No double-recreation risk. OffsetReader correct. No performance regression
(recreation only on variant change + seek, not sequential playback).

**Important correction:** The 2 unit tests mentioned in Round 3 as asserting buggy behavior
do NOT exist in the codebase. The fix can be applied without breaking any current tests.

## Why v1/v2 Was Wrong

### Three Incorrect Assumptions

1. **"IsoMp4Reader seek is broken for virtual byte layout"** — Wrong. It works perfectly
   when given the correct context. The bug was reusing a reader across variant switches.

2. **"Point fixes create new edge cases"** — Wrong. Prior fixes treated symptoms (SeekLayout,
   variant_fence, seek_epoch) without identifying the root cause (line 658). The architecture
   is robust once the root cause is fixed.

3. **"28+ shared atomics are the fundamental problem"** — Wrong. The atomics work correctly.
   The failures were high-level logic errors (wrong version of data), not low-level races.

### The Debugging Lesson

The error `seek past EOF: new_pos=1162117088 len=1860569` was misread as "Symphonia computed
an impossible offset for this file." Correct reading: "A decoder built for variant A (1.16GB)
tried to seek in variant B (1.86MB)." The tool reported the error correctly — kithara failed
to recreate the decoder.

**This is a textbook case of blaming the library for a state management failure in the application.**

### Cost Comparison

| Approach | Lines changed | Time | Risk |
|----------|--------------|------|------|
| v1/v2 actor pipeline | ~6000 new, ~2500 deleted | 8-12 weeks | High (valley of death) |
| v3 surgical fixes | ~50 changed | 2-3 days | Minimal |

## The 5 Bugs — Complete Fix Plan

### Bug 1: Missing `variant_changed` [CRASH] — Priority: IMMEDIATE

**File:** `crates/kithara-audio/src/pipeline/source.rs:658`
```rust
// BEFORE (buggy):
if !codec_changed && !stale_base_offset { return true; }

// AFTER (fixed):
if !codec_changed && !variant_changed && !stale_base_offset { return true; }
```

### Bug 2: Readiness Probe Overshoot [FILE HANG] — Priority: FAST FOLLOW

**File:** `crates/kithara-audio/src/pipeline/source.rs:1155`
Clamp 32KB probe against `len()` for non-segmented sources.

### Bug 3: EOF Delivery Stall [WORKER HANG] — Priority: FOLLOW-UP

**File:** `crates/kithara-audio/src/pipeline/audio_worker.rs:248-258`
Retry `pending_fetch` before `AtEof` early return.

### Bug 4: Hang Detector False Positive — Priority: FOLLOW-UP

**File:** `crates/kithara-stream/src/backend.rs`
Add `hang_reset!()` in idle/throttle waits.

### Bug 5: File Range Refill After Commit — Priority: FOLLOW-UP

Allow range refills for committed resources.

## What Is Verified Correct (Do NOT Change)

- FetchManager, AssetStore, PlaylistState
- Virtual byte layout (correct for sequential playback)
- ReadSeekAdapter seek toggle
- Epoch mechanism (rapid seeks handled properly)
- SeekLayout::Preserve/Reset classification
- OffsetReader byte translation
- DRM/PKCS7 transparent decryption

## v3 Design Principles

1. **Trust Symphonia** — IsoMp4Reader handles fMP4 correctly
2. **One reader per variant lifecycle** — recreate on ANY variant change
3. **Virtual byte layout is sound** — only fails with stale readers
4. **Surgical fixes over rewrites** — each bug has a precise location and fix
5. **Preserve working code** — no deletions of battle-tested infrastructure

## Implementation Order

| Phase | Bug | Time | Impact |
|-------|-----|------|--------|
| v3.0 | Bug 1 (variant_changed) | 1 day | Fixes HLS seek crash |
| v3.1 | Bug 2 (readiness probe) | 0.5 day | Fixes 8 file test hangs |
| v3.2 | Bugs 3-5 (worker/backend) | 1-2 days | Fixes remaining failures |
