# Architecture v3: Symphonia-First Surgical Fixes

> Result of 5-round architecture review debate (Codex + Gemini + Claude).
> Replaces v1 (actor pipeline) and v2 (actor pipeline refined).
> Previous designs: `.docs/actor-pipeline-design.md` (v1), `.docs/actor-pipeline-design-v2.md` (v2)
> Debate: `.docs/debate-v3/`

## Executive Summary

The HLS seek bug (`seek past EOF: new_pos=1162117088 len=1860569`) is NOT a Symphonia bug
and does NOT require an architectural rewrite. It is caused by 5 distinct lifecycle/logic
bugs in kithara, each with a surgical fix. Total effort: ~50 lines, 2-3 days.

The v1/v2 actor pipeline approach was based on three incorrect assumptions:
1. That IsoMp4Reader's seek is broken for virtual byte layouts (it isn't)
2. That point fixes create new edge cases (the root cause was never found until now)
3. That 28+ shared atomics are a fundamental problem (they work correctly)

## Design Philosophy

**Work WITH Symphonia, not around it.**

- Symphonia's IsoMp4Reader 0.6-dev natively supports fragmented MP4 (MoofSegment chaining,
  incremental moof discovery, seek_track_by_ts across segments)
- The virtual byte layout (init + segments concatenated) is correct for sequential playback
- The SeekLayout::Preserve/Reset classification is the right framework
- FetchManager, AssetStore, PlaylistState are all correct and battle-tested

## Root Cause Analysis

### The Primary Bug (seek-past-EOF crash)

**Location:** `crates/kithara-audio/src/pipeline/source.rs` line 658

```rust
fn align_decoder_with_seek_anchor(&mut self, anchor: &SeekAnchor) -> bool {
    let codec_changed = /* ... checks AudioCodec */;
    let variant_changed = /* ... checks variant_index */;  // COMPUTED
    let stale_base_offset = self.base_offset > 0;

    if !codec_changed && !stale_base_offset {
        return true;  // BUG: variant_changed is IGNORED
    }
    // ... decoder recreation path
}
```

**Scenario:** ABR switches from variant 0 (AAC 66k) to variant 1 (AAC 270k):
- `codec_changed = false` (both AAC)
- `variant_changed = true` (0 → 1) — **computed but not checked**
- `stale_base_offset = false` (base_offset == 0, no prior switch)
- Guard evaluates to `true` → early return → decoder NOT recreated
- Old IsoMp4Reader has variant 0's moof_base_pos values (gigabytes)
- Seek computes position in old layout → "seek past EOF"

**Fix:** `if !codec_changed && !variant_changed && !stale_base_offset { return true; }`

### How IsoMp4Reader Actually Works

1. During init: parses ftyp + moov + first moof+mdat pair
2. During playback: `next_packet()` calls `try_read_more_segments()` to lazily discover
   subsequent moof atoms. Each MoofSegment records `moof_base_pos` = absolute byte position
3. During seek: `seek_track_by_ts()` iterates known segments, reads more if needed,
   computes sample byte position from `moof_base_pos + data_offset`
4. All byte positions are **baked in at parse time** — valid only while layout is stable

**This means:** Same-variant seek works (layout unchanged, positions valid).
Cross-variant seek with old reader fails (positions reference old layout).

## Complete Bug List

### Bug 1: Missing `variant_changed` Check — CRASH FIX

**File:** `crates/kithara-audio/src/pipeline/source.rs:658`

```rust
// Fix: add && !variant_changed
if !codec_changed && !variant_changed && !stale_base_offset {
    return true;
}
```

**Affects:** All HLS seek-after-ABR-switch scenarios
**Tests:** HLS stress tests with ABR variant switching

### Bug 2: File Readiness Probe Overshoots EOF — FILE HANG FIX

**File:** `crates/kithara-audio/src/pipeline/source.rs:1155`

`is_ready()` probes `pos..pos+32768` for non-segmented sources. Small WAV files
(~4044 bytes) fail the 32KB probe. `contains_range()` does not clamp to EOF.

**Fix:** Clamp probe against `len()`:
```rust
let check_end = self.shared_stream.current_segment_range()
    .map_or_else(
        || {
            let probe = pos.saturating_add(32 * 1024);
            self.shared_stream.len().map_or(probe, |len| probe.min(len))
        },
        |seg| seg.end,
    );
```

**Affects:** 8 File playback tests (test_audio_is_eof, test_audio_read, etc.)

### Bug 3: EOF Delivery Stall in Audio Worker

**File:** `crates/kithara-audio/src/pipeline/audio_worker.rs:248-258`

When EOF fetch arrives but ringbuf full: `pending_fetch` stored, phase → `AtEof`,
but `AtEof` returns `NoProgress` before retrying pending_fetch.

**Fix:** Retry `pending_fetch` before `AtEof` early return.

### Bug 4: Downloader Hang Detector False Positive

**File:** `crates/kithara-stream/src/backend.rs`

Backend downloader idles after all data delivered. Hang detector fires on idle wait.

**Fix:** Reset hang detector in idle/throttle waits.

### Bug 5: File Range Refill After Commit

Gap-fill writes to committed File resources fail.

**Fix:** Allow specific range refills for committed resources.

## What Is NOT Changed

| Component | Status | Why |
|-----------|--------|-----|
| FetchManager | Keep | Async fetch + cache orchestration correct |
| AssetStore | Keep | Disk/memory cache with lease/pin correct |
| PlaylistState | Keep | Variant/segment metadata resolution correct |
| Virtual byte layout | Keep | Correct for sequential playback |
| ReadSeekAdapter | Keep | Seek toggle for init-time behavior correct |
| OffsetReader | Keep | Byte translation correct |
| Epoch mechanism | Keep | Rapid seeks handled correctly |
| SeekLayout | Keep | Preserve/Reset classification correct |
| Source trait | Keep | read_at/wait_range API correct |
| Stream<T> | Keep | Read+Seek wrapper correct |
| SharedSegments | Keep | Mutex+Condvar bridge works |
| DRM/PKCS7 | Keep | Transparent decryption via ProcessChunkFn |

## Implementation Plan

| Phase | Bug | Effort | Tests Fixed |
|-------|-----|--------|------------|
| **v3.0** | Bug 1 (variant_changed) | 1 day | HLS seek-after-ABR tests |
| **v3.1** | Bug 2 (readiness probe) | 0.5 day | 8 File playback tests |
| **v3.2** | Bugs 3-5 (worker/backend) | 1-2 days | Remaining failures |

**Total: 2-3 days, ~50 lines changed, zero architectural risk.**

## Future Considerations

- **Cold-seek latency:** If seeking to distant segment after variant switch is slow
  (incremental moof scan), per-segment reader is an optimization option
- **Shared state centralization:** 21 atomics could be grouped for observability
- **Actor pipeline:** May revisit for modularity/testability, NOT for this bug

## Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Do NOT bypass FormatReader | IsoMp4Reader handles fMP4 correctly |
| 2 | Do NOT build custom atom parsing | Reinventing tested Symphonia code |
| 3 | Do NOT implement actor pipeline | 5 surgical fixes solve the problem |
| 4 | Recreate decoder on variant change | Stale moof_base_pos causes seek crash |
| 5 | Keep virtual byte layout | Correct for sequential playback |
| 6 | Keep SeekLayout classification | Preserve/Reset logic verified correct |
| 7 | Clamp readiness probe | 32KB overshoot causes file hang |
| 8 | Fix EOF delivery ordering | Pending fetch orphaned in AtEof state |

## References

- `.docs/debate-v3/round-1-findings.md` — Symphonia analysis, bug NOT in Symphonia
- `.docs/debate-v3/round-2-crossreview.md` — Minimal fix identified
- `.docs/debate-v3/round-3-deep-analysis.md` — 5 bugs found
- `.docs/debate-v3/round-4-5-final-synthesis.md` — Fix validated, v1/v2 postmortem
- `.docs/architecture-seek-fsm-analysis.md` — Prior FSM analysis (28+ shared state)
- `.docs/offset-architecture-design.md` — SeekLayout design (correct framework)
