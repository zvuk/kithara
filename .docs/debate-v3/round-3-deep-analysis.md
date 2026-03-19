# Round 3: Deep Analysis — 5 Distinct Bugs Found

## Critical Discovery: Not 1 Bug, but 5

Codex found that the 16 failing tests are caused by AT LEAST 5 distinct bugs.
The `variant_changed` bug is the most visible but NOT the only issue.

## Bug 1: Missing `variant_changed` Check (Seek-Past-EOF Crash)

**Location:** `crates/kithara-audio/src/pipeline/source.rs` line 658
**Fix:** `if !codec_changed && !variant_changed && !stale_base_offset { return true; }`
**Impact:** HLS seek after same-codec ABR switch crashes
**Tests affected:** Category B (HLS stress tests with ABR)
**Note:** 2 existing unit tests assert the BUGGY behavior and must be rewritten:
- `seek_anchor_keeps_decoder_when_variant_changes_with_same_codec`
- `stress_variant_only_seeks_do_not_recreate_decoder`

## Bug 2: File Readiness Probe Overshoots EOF

**Location:** `crates/kithara-audio/src/pipeline/source.rs` line 1155
**The bug:** `is_ready()` probes `pos..pos+32768` (32KB) for non-segmented sources.
For small local WAV files (~4044 bytes), probe extends past EOF.
`FileSource::is_range_ready()` → `Resource::contains_range()` returns false for `0..32768`
when only `0..4044` is available.
**Fix:** Clamp the 32KB probe against `len()`, or make `contains_range()` clamp when committed
**Impact:** 8 File playback tests hang forever
**Tests:** test_audio_is_eof, test_audio_preload_*, test_audio_read*

## Bug 3: EOF Delivery Stall in Audio Worker

**Location:** `crates/kithara-audio/src/pipeline/audio_worker.rs` lines 248-258
**The bug:** When EOF fetch arrives but ringbuf is full:
1. `try_push(fetch)` fails → stored in `pending_fetch`
2. `is_eof=true` → `TrackPhase::AtEof`
3. Next iteration: `AtEof` returns `NoProgress` at line 204 BEFORE retrying pending_fetch
4. EOF fetch orphaned forever
**Fix:** Retry `pending_fetch` before the `AtEof` early return, or don't transition
to AtEof until EOF fetch is actually delivered

## Bug 4: Downloader Hang Detector False Positive

**Location:** `crates/kithara-stream/src/backend.rs`
**The bug:** Downloader loop legitimately idles after all data delivered.
Hang detector fires on idle wait, cancels token. Next read sees `Cancelled`.
**Fix:** Reset hang detector in idle/throttle waits

## Bug 5: File Range Refill After Commit

**The bug:** Gap-fill writes to already-committed File resources fail.
**Impact:** MP3 stress tests (live_stress_real_mp3_*)

## Gemini's Verification of Existing Mechanisms

### SeekLayout classify_seek(): CORRECT
- No false-Preserve cases found
- After Reset: DownloadState empty → `current_layout_variant()` returns None → Reset
- classify_seek is robust

### OffsetReader: CORRECT
- `seek(Start(pos))` translates to `base_offset + pos` correctly
- IsoMp4Reader's moof_base_pos is relative to OffsetReader's byte 0
- The bug is NOT in OffsetReader — it's that OffsetReader is never created (early return)

### Seek Flow Ordering: SAFE
- After apply_seek_plan(Reset) clears DownloadState
- media_info() falls back to playlist metadata — always returns correct info
- format_change_segment_range() also falls back to metadata offsets

### Epoch Mechanism: CORRECT
- Rapid seeks: only latest epoch processed
- `complete_seek()` double-checks epoch hasn't advanced
- `seek_target_ns` stores only latest target (correct — intermediates skipped)

## Consensus: Fix Strategy

### Immediate (v3.0): Fix Bug 1
- Add `&& !variant_changed` to align_decoder_with_seek_anchor()
- Rewrite 2 unit tests that assert buggy behavior
- Add regression test: same-codec ABR switch + seek

### Fast follow (v3.1): Fix Bug 2
- Clamp readiness probe in is_ready() against len()
- Fixes 8 File playback test hangs

### Follow-up (v3.2): Fix Bugs 3-5
- Audio worker EOF delivery
- Downloader hang detector
- File range refill

### NOT needed: Full actor pipeline rewrite
Both reviewers agree: the actor pipeline (v1/v2) was an overreaction to what
is fundamentally a lifecycle management bug. Symphonia works correctly when
used correctly.

## Test Strategy

**Tier 1 — Unit test:** Mock StreamAudioSource with variant 0→1 same-codec, base_offset=0.
Assert decoder recreation triggered.

**Tier 2 — Integration:** Real AAC HLS data (slq+smq), ABR switch, seek. No seek-past-EOF.

**Tier 3 — Stress:** Existing `stress_rapid_seeks_during_abr_switch_must_not_kill_audio`
should pass after fix.

## Key Takeaway

The v1/v2 approach (bypass FormatReader, custom fMP4 parsing, actor pipeline) was
solving the wrong problem. Symphonia's IsoMp4Reader works correctly for fMP4.
The real bugs are in kithara's lifecycle management:
1. Not recreating decoder on variant change
2. Readiness probe overshoot
3. EOF delivery race
4. Hang detector false positive
5. File resource write after commit
