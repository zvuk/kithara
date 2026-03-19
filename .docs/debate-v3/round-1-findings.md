# Round 1: Symphonia-First Architecture Investigation

## Unanimous Finding

**The bug is NOT in Symphonia's IsoMp4Reader. It is in kithara's usage.**

IsoMp4Reader 0.6-dev fully supports fragmented MP4 (fMP4) via:
- `MoofSegment` chaining in `try_read_more_segments()`
- Incremental moof atom discovery during `next_packet()`
- `seek_track_by_ts()` iterating over all known segments

The bug: kithara reuses a long-lived IsoMp4Reader across seek operations when the
underlying virtual byte layout has been invalidated (ABR variant switch, DownloadState clear).
IsoMp4Reader's internal `self.segs` cache byte positions from parse time — after layout change,
these positions point to non-existent locations.

## Codex Findings

### IsoMp4Reader Seek Mechanics (0.6-dev)
- **Init with seekable=true**: scans ALL atoms end-to-end. Kithara disables seek during init.
- **Init with seekable=false**: reads atoms sequentially to first moof/mdat. Discovers more lazily.
- Each `MoofSegment` records `moof_base_pos` = absolute byte position at parse time.
- `seek()` does NOT call underlying `Seek::seek()` — only updates internal track state.
- `next_packet()` later seeks to `sample_info.pos` before reading.
- All byte positions are **baked in at parse time**.

### Root Cause Chain
1. Decoder built on variant A (total virtual size ~1.16GB)
2. ABR switch to variant B (total ~1.86MB)
3. `align_decoder_with_seek_anchor()`: codec_changed=false, stale_base_offset=false → no recreation
4. Old IsoMp4Reader seeks to 1.16GB (valid in old layout, invalid in new)
5. `Stream::seek()` checks new_pos > len → "seek past EOF"

### Recommendation
- One IsoMp4Reader per variant lifecycle
- Remove OffsetReader for fMP4 — each decoder gets fresh stream from byte 0
- Same-variant seek: keep existing reader (layout preserved, moof positions valid)
- Cross-variant seek: always recreate decoder

## Gemini Findings

### MediaSourceStream Analysis
- MSS trusts inner source's `seek()` return value absolutely
- After seek, `abs_pos` set to whatever inner seek returned
- Every seek invalidates entire read-ahead buffer
- IsoMp4Reader tries `seek_buffered()` first, falls back to full seek

### Architecture Options
1. **Option A (recommended)**: One IsoMp4Reader per variant lifecycle, continuous virtual stream
2. **Option B**: Per-segment FormatReader (ExoPlayer-style)
3. **Option C (rejected)**: Direct codec without FormatReader (v1/v2 approach)

### Industry Comparison
- **ffmpeg**: Concatenates segments, feeds to mov demuxer. New demuxer on variant switch. ≈ Option A.
- **ExoPlayer**: Per-segment `FragmentedMp4Extractor`. ≈ Option B.
- **AVPlayer**: System-level, per-segment parsing internally.
- **MSE (web)**: Appends segments to SourceBuffer, browser handles internally.

## Claude Findings

### Current Architecture Data Flow
```
PlaylistState → HlsDownloader → AssetStore → DownloadState
→ HlsSource (read_at) → Stream<Hls> (Read+Seek) → SharedStream
→ OffsetReader → ReadSeekAdapter → IsoMp4Reader → Codec → PCM
```

### What Works vs What Doesn't
- ✅ Sequential playback (no seek) — moof positions valid, incremental discovery works
- ✅ File playback — single contiguous resource
- ⚠️ Same-variant seek (Preserve path) — works if decoder's moof index not invalidated
- ❌ Seek after ABR variant switch — stale moof positions in old decoder
- ❌ DRM path — PKCS7 padding changes segment sizes, metadata/committed space diverge
- ❌ Rapid seek stress tests — 7 integration tests fail

### Bug Root Cause
The `stale_base_offset` check in `align_decoder_with_seek_anchor()` catches variant switches
via non-zero `base_offset`, but misses the case where:
- `base_offset == 0` (no prior switch)
- Variant changed but codec is the same (AAC 66k → AAC 270k)
- DownloadState cleared but decoder not recreated

### Fix Options (within FormatReader constraint)
1. Better decoder recreation triggers (extend stale_base_offset to cover ALL variant changes)
2. Per-segment decoder creation (each segment = init + media, fresh reader)
3. Freeze layout per decoder, invalidate on any change

## Key Consensus Points

| Point | Codex | Gemini | Claude |
|-------|-------|--------|--------|
| Bug in Symphonia? | No | No | No |
| fMP4 support in 0.6-dev? | Yes, native | Yes, native | Yes, native |
| v1/v2 custom parsing needed? | No | No | No |
| Correct approach? | Option A | Option A | Option A or B |
| Recreate on variant switch? | Always | Always | Always |
| OffsetReader for fMP4? | Remove | N/A | Problematic |
