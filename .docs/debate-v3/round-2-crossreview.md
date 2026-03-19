# Round 2: Cross-Review

## Critical Finding: Minimal Fix Exists

Both Codex and Gemini independently identified the EXACT bug location and agree
on a minimal surgical fix — NO full rewrite needed.

### The Bug (confirmed in code)

`/crates/kithara-audio/src/pipeline/source.rs` line 658:
```rust
let variant_changed = matches!((current_variant, target_variant), (Some(from), Some(to)) if from != to);
// ...
if !codec_changed && !stale_base_offset {
    return true; // BUG: variant_changed computed but NOT CHECKED
}
```

When ABR switches between same-codec variants (AAC 66k → AAC 270k):
- `codec_changed = false`
- `variant_changed = true` (IGNORED)
- `stale_base_offset = false` (base_offset == 0 on first switch)
- Decoder NOT recreated → stale moof_base_pos → "seek past EOF"

### The Minimal Fix

```rust
if !codec_changed && !variant_changed && !stale_base_offset {
    return true;
}
```

### Codex Cross-Review

**Agrees with diagnosis. Additional gap found:**
- `HlsSource::read_at()` also allows same-codec cross-variant reads silently
  via `can_cross_variant_without_reset()` — should force VariantChange instead
- ReadSeekAdapter: keep `seek_enabled` toggle, shrink OffsetReader to fallback
- Actors NOT needed for this fix — centralize state machine first, actors later if ever
- Minimal fix has low regression risk

### Gemini Cross-Review

**Agrees with diagnosis. Key technical findings:**

**Option B (per-segment reader) is impractical:**
- Loses codec internal state (delay buffers, overlap-add)
- AAC needs ~2048 samples priming after reset → audible clicks at every segment boundary
- No way to separate codec from FormatReader for fMP4 in Symphonia

**Option A incremental scan latency:**
- mdat bodies skipped via `seek(Current(delta))` — no download needed
- BUT moof headers (~8 bytes) at start of each segment DO require segment download
- Seek to segment 30 after variant switch: potentially 24 sequential downloads
- Only affects same-variant seeks before first ABR switch
- After first ABR switch, `stale_base_offset > 0` → always recreates decoder

**Hybrid approach collapses to Option A:**
- Seek-scoped reader + transition to continuous = just Option A with recreation on seek
- This is already what happens after first ABR switch via stale_base_offset

**DRM (PKCS7) handled correctly:**
- `ProcessChunkFn` in AssetStore decrypts transparently
- `LoadedSegment.media_len` reflects post-decryption size
- Virtual byte stream sees correct decrypted bytes

**Shared atomics (21 counted):**
- Timeline: 11 atomics
- SharedSegments: Mutex, Condvar, Notify, 2x AtomicBool, AtomicU32, AtomicUsize, SegQueue
- Option A does NOT reduce this count
- Virtual byte stream inherently requires shared state

## Consensus Table

| Question | Codex | Gemini | Claude |
|----------|-------|--------|--------|
| Minimal fix correct? | Yes | Yes | Yes |
| Option A or B? | A (default) | A (B impractical) | A |
| Actors needed? | No | No | No |
| OffsetReader needed? | Shrink to fallback | Not needed after fix | Reassess |
| DRM handled? | Yes | Yes, transparently | Yes |
| Incremental scan problem? | Performance only | Real but bounded | Separate perf concern |

## Outstanding Questions for Round 3

1. The minimal fix fixes the crash, but are there OTHER failure modes in the 84 failing tests?
2. Should we also fix `HlsSource::read_at()` cross-variant reads (Codex's gap)?
3. What about the SeekLayout::Preserve/Reset — is it correct as-is, or needs tweaks?
4. The OffsetReader — when exactly does base_offset become stale, and is the detection reliable?
5. Can we write a regression test that reproduces the exact "seek past EOF" scenario?
