# v4 Debate — Rounds 4-5: Final Synthesis

## Unanimous Verdict: Structural Refactoring, 7 Steps, ~5-7 Days

Both Codex and Gemini validate the Round 3 merged design and confirm it addresses all 9 bugs
from the Hydra. The design moves the pipeline from reactive "detect and fix" to proactive
"plan and execute."

## Hydra Validation — All 9 Bugs Addressed

| # | Bug | Fix Mechanism | Status |
|---|-----|--------------|--------|
| 1 | Missing `variant_changed` check (L658) | `evaluate_recreation()` unified decision | **FIXED** |
| 2 | DRM PKCS7 offset divergence | `MetadataOffset`/`CommittedOffset` types | **FIXED** |
| 3 | Sparse MediaInfo | `MediaInfo::merge` preventing split-brain | **FIXED** |
| 4 | `Audio::new()` losing variant_index | `MediaInfo::merge` at construction | **FIXED** |
| 5 | Init-shape loss on re-download | Downloader shape contract (`require_init`) | **FIXED** |
| 6 | Stale-base recreate | `evaluate_recreation()` StaleBaseOffset | **FIXED** |
| 7 | `Backend::drop` not joining | Patch 0: add `.join()` to Drop | **FIXED** |
| 8 | `wait_range` spinning | Typed offsets + committed-offset-authoritative lookup | **FIXED** |
| 9 | Decode-forward wrong offset | `AfterRecreate::DecodeForward` + `SeekSession.remaining_skip` | **FIXED** |

## Remaining Risks

1. **Downloader Shape State** — Relying on "contract" rather than InitStore places burden on
   demand-planning logic. Must ensure re-downloaded segments cannot "downgrade" existing init-bearing entries.
2. **WASM Sync Constraints** — Backend::drop join fix needs careful isolation to avoid blocking
   main thread on WASM.
3. **State Machine Complexity** — `DecoderLifecycle` transition from Seeking→Ready ("first visible
   chunk" signal) is most likely regression source.
4. **Offset Newtypes Churn** — Broad compile churn if not kept internal to kithara-hls initially.
5. **Merge Conflicts** — Steps 3-7 touch overlapping files; parallel branches would be painful.

## Final Implementation Plan

### Step 0: Backend Safety Patch
- **Files:** `crates/kithara-stream/src/backend.rs`
- **Change:** Add `.join()` in `Drop::drop()` after `cancel.cancel()`
- **Tests:** Add `drop_waits_for_worker_exit`; rerun DRM module subset
- **Effort:** 1-2h | **Risk:** Low | **Depends:** None

### Step 1: MediaInfo Merge Foundation
- **Files:** `crates/kithara-stream/src/media.rs`, `crates/kithara-audio/src/pipeline/audio.rs`, `crates/kithara-audio/src/pipeline/source.rs`
- **Change:** Add `MediaInfo::merge()`, replace `or_else` in `Audio::new()`
- **Tests:** Unit tests for merge, `resolve_initial_media_info_preserves_stream_variant_for_user_hints`
- **Effort:** 2-4h | **Risk:** Low | **Depends:** Step 0

### Step 2: Typed Offset Spaces
- **Files:** `crates/kithara-hls/src/playlist.rs`, `crates/kithara-hls/src/source_wait_range.rs`, `crates/kithara-hls/src/source.rs`, `crates/kithara-hls/src/downloader.rs`
- **Change:** Internal `MetadataOffset`/`CommittedOffset` newtypes, convert critical crossings
- **Tests:** Metadata-vs-committed lookup regressions, DRM shape change tests
- **Effort:** 6-8h | **Risk:** High (compile churn) | **Depends:** Step 0

### Step 3: Enriched Immutable Seek Anchor
- **Files:** `crates/kithara-stream/src/source.rs`, `crates/kithara-stream/src/stream.rs`, `crates/kithara-hls/src/source.rs`
- **Change:** Add `media_info`, `recreate_offset`, `layout` to `SourceSeekAnchor`; make `build_seek_plan()` the single builder
- **Tests:** Update seek layout tests, add `build_seek_plan` coverage
- **Effort:** 4-8h | **Risk:** Medium (cross-crate contract) | **Depends:** Steps 1-2

### Step 4: Downloader Init-Shape Contract
- **Files:** `crates/kithara-hls/src/source.rs`, `crates/kithara-hls/src/downloader.rs`
- **Change:** Add `require_init` to `SegmentRequest`, fix `commit_segment()` shape derivation, `segment_loaded_for_demand()` rejects degraded shape
- **Tests:** `segment_loaded_for_demand_requires_init_after_nonzero_seek`, `build_demand_plan_preserves_init_shape`
- **Effort:** 3-8h | **Risk:** Medium | **Depends:** Steps 2-3

### Step 5: Centralized Recreation Policy
- **Files:** `crates/kithara-audio/src/pipeline/source.rs`
- **Change:** Implement `evaluate_recreation()`, collapse 4 recreation paths into one, add `DecoderLifecycle` struct with `DecoderMode` enum
- **Tests:** variant-switch recreate, WAV nonzero-seek, DRM seek, `apply_time_anchor_seek_refreshes_anchor_after_failure`
- **Effort:** 6-12h | **Risk:** High (core logic rewrite) | **Depends:** Steps 1-3

### Step 6: SeekSession and AfterRecreate
- **Files:** `crates/kithara-audio/src/pipeline/source.rs` (or new `decoder_lifecycle.rs`)
- **Change:** Replace 6 Option fields with `SeekSession`, implement `AfterRecreate` enum, implement state transitions
- **Tests:** `retry_decode_failure_after_seek_recreate_preserves_original_target`, decode-forward regressions, `live_ephemeral_revisit_sequence_regression`
- **Effort:** 6-10h | **Risk:** High | **Depends:** Step 5

### Step 7: Soak and Regression Sweep
- **Files:** Test targets in `tests/tests/kithara_hls/`
- **Change:** Full regression sweep, stress tests, DRM tests
- **Tests:** All `stress_chunk_integrity`, `live_stress_real_stream`, HLS seek tests
- **Effort:** 3-5h | **Risk:** Medium | **Depends:** All prior

### Total Effort: ~31-57 engineer-hours (~5-7 working days)

## Final Answers

### 1. Typed Offsets — In v4 Round 1, NOT Deferred
Both agree: typed offsets must be in v4. They are the only way to permanently fix the DRM/HLS
divergence (bugs 2 and 8). Keep internal to `kithara-hls` to limit compile churn.

### 2. Minimum Viable v4
The Hydra-proof core: Patch 0 + MediaInfo::merge + typed offset spaces + enriched immutable
SourceSeekAnchor + downloader require_init shape contract + one centralized evaluate_recreation()
+ one SeekSession owner with AfterRecreate.

A separate DecoderLifecycle file is optional; a single owner for that state is not.

### 3. Git Strategy
- **One small PR for Patch 0** (standalone, independent)
- **One feature branch `feat/v4-architecture`** for Steps 1-7
- One commit per step, all test-passing
- Do NOT use parallel feature branches — merge-conflict rate will be high on shared files

## Design Principles (Reiterated)

1. **Trust Symphonia** — IsoMp4Reader handles fMP4 correctly
2. **One reader per variant lifecycle** — recreate on ANY variant change
3. **Immutable plans over mutable re-queries** — SourceSeekAnchor is the single truth
4. **Typed offset spaces** — MetadataOffset/CommittedOffset prevent split-brain at compile time
5. **Be dumber on WHEN to recreate, careful on HOW** — evaluate_recreation() is aggressive, AfterRecreate handles mechanics
6. **Preserve working code** — no deletions of battle-tested FetchManager, AssetStore, PlaylistState

## References

- `.docs/debate-v4/round-1-findings.md` — Independent analyses
- `.docs/debate-v4/round-2-crossreview.md` — Cross-review consensus
- `.docs/debate-v4/round-3-deep-analysis.md` — Concrete merged design
- `.docs/debug-journal-2026-03-09.md` — The Hydra (9 bugs, 3 days)
- `.docs/actor-pipeline-design-v3.md` — v3 surgical fixes (superseded by v4)
