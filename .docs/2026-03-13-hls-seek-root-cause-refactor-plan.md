# HLS Seek Root-Cause Refactor Plan

**Goal:** remove seek/recovery compensations and restore a clean contract where the real landed decoder position, not the speculative anchor, drives HLS source state and segment demand.

**What is already proven**
- Decoder recovery/fallback was masking the bug instead of fixing it.
- Same-codec ABR switch must not recreate the decoder.
- `symphonia.seek(time)` can legally land before the requested anchor segment boundary.
- The current code commits HLS state to the anchor too early, then tries to compensate with queue dedupe, pending state, and worker-side retries.
- Queue flooding was secondary. The primary bug is the mismatch between planned anchor and actual landed byte position.

## Design Invariants

- `SourceSeekAnchor` is planning data only. It is not the committed reader position.
- Same-codec ABR switch does not recreate the decoder.
- Codec change is the only normal reason to recreate the decoder.
- The first authoritative post-seek byte offset is the actual landed stream position after `decoder.seek(...)` returns.
- HLS segment demand is derived from one source of truth, not from both a FIFO queue and a shadow `pending_*` field.
- HLS anchor-resolution failure is not silently downgraded into a direct seek.
- `TrackState` is the only owner of cross-layer seek workflow state.
- HLS source and downloader may keep source-local I/O state, but they must not own hidden workflow phases such as "seek requested", "seek in progress", "awaiting resume", or "retry pending".

## FSM Ownership Guardrails

These rules are mandatory for the refactor.

### 1. `TrackState` owns workflow, not HLS internals

Allowed in `TrackState`:
- seek requested
- applying seek
- awaiting post-seek readiness / resume
- recreating decoder
- decoding
- failed

Not allowed in HLS/source/downloader internals:
- hidden pending seek flags
- implicit "we already applied this seek" markers
- alternate workflow branches encoded via queue contents

If a state transition matters to both audio and HLS, it must be represented in `TrackState`, not inferred from source-local fields.

### 2. HLS source keeps only source-local facts

Allowed in HLS:
- committed loaded segments
- current variant fence
- source-local demand lifecycle
- current landed byte position once committed

Not allowed in HLS:
- speculative reader position committed before `decoder.seek(...)` lands
- shadow workflow state that duplicates `TrackState`
- "pending recover", "pending resume", "pending fallback", or equivalent fields

### 3. Downloader reacts to demand state, it does not define workflow

The downloader may know:
- what segment is demanded
- whether it is queued / in flight / committed / stale

The downloader must not be responsible for deciding:
- whether seek is complete
- whether decoder resume should happen
- whether a failed seek should fall back to another mode

### 4. One transition direction

Cross-layer flow must remain one-way:
- `TrackState` decides the workflow phase
- source/HLS execute source-local operations for that phase
- source/HLS return facts back to the audio layer
- `TrackState` advances

Not allowed:
- HLS mutates shared state in a way that implicitly advances the audio FSM
- audio infers workflow state from queue internals instead of explicit `TrackState`

## Root Causes To Eliminate

### 1. Anchor resolution and state mutation are fused

Current problem:
- `HlsSource::seek_time_anchor()` in `crates/kithara-hls/src/source.rs` resolves the anchor and immediately calls `apply_seek_plan()`.
- `apply_seek_plan()` mutates queue state, `Timeline.byte_position`, and `current_segment_index` before the decoder has actually sought.

Why this is wrong:
- Symphonia seek is time-based and may land earlier than the requested segment boundary.
- Once HLS state is committed to the anchor prematurely, every later readiness/range/request decision is operating on fiction.

Target:
- Split seek into two explicit phases:
  1. resolve target anchor
  2. commit actual landed position

### 2. Post-seek landed position is not reconciled back into HLS

Current problem:
- `StreamAudioSource` seeks the decoder using `anchor.segment_start`, but there is no explicit post-seek reconciliation step that tells HLS where the decoder actually landed.
- `source_is_ready()`, `current_segment_range()`, `step_awaiting_resume()`, and on-demand request logic all consume state that may still describe the anchor, the old segment, or stale committed layout.

Target:
- After `decoder.seek(...)` succeeds, the audio layer must treat `shared_stream.position()` as authoritative and explicitly commit that landing back to HLS state before resuming decode.

### 3. Demand lifecycle is duplicated across queue + shadow pending state

Current problem:
- `segment_requests` and `pending_segment_request` encode the same lifecycle in two places.
- `poll_demand_impl()` has to manually clear pending state on many independent early-return paths.
- This is a classic sign that ownership is unclear.

Target:
- Replace the current queue + shadow field combination with one authoritative demand lifecycle.
- End state must not contain `pending_segment_request` or equivalent `pending_*` shadows for segment demand.

### 4. Audio seek path still contains fallback-oriented control flow

Current problem:
- `apply_seek_from_timeline()` still treats `seek_time_anchor()` failure as a warning and falls back to direct seek.
- `apply_anchor_seek_with_fallback()` still encodes the old philosophy in both name and shape.
- Decode error handling still contains recovery-oriented structure even where we now know those errors should surface.

Target:
- For HLS anchor path: no silent downgrade to direct seek.
- Seek failure becomes a real track failure unless it is a legitimate codec-change recreation path.

## Target Architecture

### A. Two-phase seek contract

Introduce an explicit seek lifecycle:
- `resolve_seek_anchor(target_time) -> SourceSeekAnchor`
- `prepare_seek(anchor, epoch)` for source-local staging only
- `decoder.seek(anchor.segment_start)`
- `commit_seek_landing(epoch, landed_offset, anchor)` to reconcile source state to the actual landed position

Rules:
- `prepare_seek(...)` may clear stale requests or stage target-variant context.
- `prepare_seek(...)` must not commit `Timeline.byte_position` to the anchor.
- `commit_seek_landing(...)` is the only place allowed to commit post-seek byte position and current segment hint.

### B. One-way state flow

The post-seek flow must become:
- target time
- anchor resolution
- decoder seek
- actual landed offset
- HLS reconciliation
- readiness check
- first decoded chunk

Not:
- target time
- anchor resolution
- optimistic anchor commit
- retries / dedupe / recoveries / fallback seek

### C. One demand owner

Segment demand should have one owner and one lifecycle.

Acceptable end states:
- a small `DemandState` enum with explicit transitions such as `Idle`, `Queued`, `InFlight`
- or a queue primitive that enforces uniqueness itself and does not require an extra `pending_*` shadow field

Unacceptable end state:
- manual `clear_pending_*` calls scattered across downloader branches

## Refactor Sequence

### Phase 0. Freeze the contracts with tests

Modify:
- `tests/tests/kithara_hls/source_internal_cases.rs`
- `tests/tests/kithara_hls/stress_seek_lifecycle.rs`
- `tests/tests/kithara_audio/stream_source_tests.rs`

Add or update tests for these invariants:
- same-codec ABR seek does not recreate decoder
- landed offset before anchor is legal and must resume playback correctly
- HLS does not commit reader position to anchor before decoder landing is known
- HLS on-demand logic does not require shadow `pending_segment_request`
- anchor failure does not silently fall back to direct seek for HLS

This phase is critical because the current code has too many coupled moving parts to safely refactor without contract tests.

### Phase 1. Separate seek planning from seek commitment

Modify:
- `crates/kithara-hls/src/source.rs`
- `crates/kithara-stream/src/source.rs`
- `crates/kithara-stream/src/stream.rs`

Refactor:
- Make anchor resolution pure or near-pure.
- Remove the eager `Timeline.byte_position` update from `apply_seek_plan()`.
- Replace `apply_seek_plan()` with two functions:
  - planning/staging
  - landed-position commit

Expected outcome:
- After `seek_time_anchor()`, HLS has target context but has not lied about the actual reader position yet.

### Phase 2. Reconcile landed offset explicitly

Modify:
- `crates/kithara-audio/src/pipeline/source.rs`
- `crates/kithara-hls/src/source.rs`

Refactor:
- After `decoder.seek(anchor.segment_start)` succeeds, read the actual landed stream position.
- Commit that landed offset back into HLS using an explicit method.
- Derive current segment range, current segment hint, and initial demand from the landed offset, not from the anchor offset.

Expected outcome:
- `source_is_ready()`, `current_segment_range()`, and `step_awaiting_resume()` observe real post-seek state.
- The first post-seek wait/read requests the segment covering the landed offset, even when it is earlier than the anchor segment.

### Phase 3. Remove shadow demand state

Modify:
- `crates/kithara-hls/src/source.rs`
- `crates/kithara-hls/src/downloader.rs`

Refactor:
- Delete `pending_segment_request`.
- Move demand uniqueness/ownership into one place.
- Collapse duplicated stale/skip/commit cleanup into a single lifecycle API.

Possible shape:
- `DemandState::Idle`
- `DemandState::Queued(SegmentRequest)`
- `DemandState::InFlight(SegmentRequest)`

The exact enum can differ, but the ownership rule must be obvious:
- enqueue in one place
- transition to in-flight in one place
- finish or drop in one place

Expected outcome:
- `poll_demand_impl()` becomes a straight-line function again.
- No more repeated `clear_pending_segment_request(...)` calls.

### Phase 4. Remove fallback-oriented seek logic from audio pipeline

Modify:
- `crates/kithara-audio/src/pipeline/source.rs`
- `crates/kithara-audio/src/pipeline/track_fsm.rs`

Refactor:
- Remove direct-seek fallback on HLS anchor-resolution failure.
- Rename or delete `apply_anchor_seek_with_fallback()`.
- Keep decoder recreation only for codec change / true format boundary.
- Make failures explicit in `TrackState::Failed(...)` instead of trying alternate seek modes.

Expected outcome:
- Audio seek path says exactly what it does.
- Decoder errors are no longer “explained away” by fallback branches.

### Phase 5. Optional structural cleanup after behavior is green

Only after the behavior is stable:
- extract HLS seek logic into a dedicated module such as `crates/kithara-hls/src/seek.rs`
- extract demand lifecycle into a dedicated module such as `crates/kithara-hls/src/demand.rs`

This is not for future-proofing.
It is only justified if the refactor leaves `source.rs` and `downloader.rs` too dense to reason about.

## Concrete Delete List

These items should disappear by the end of the refactor:
- `pending_segment_request` in `crates/kithara-hls/src/source.rs`
- repeated `clear_pending_segment_request(...)` branches in `crates/kithara-hls/src/downloader.rs`
- eager anchor commit of `Timeline.byte_position` in HLS seek staging
- HLS anchor error -> direct seek fallback in `crates/kithara-audio/src/pipeline/source.rs`
- misleading fallback naming in `apply_anchor_seek_with_fallback()`

## Verification Matrix

Run during the refactor:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_audio::stream_source_tests -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_hls::source_internal_cases -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`

Run before declaring success:
- the whole `tests` crate suite relevant to audio/HLS seek behavior
- at minimum all `kithara_hls::*` integration tests
- at minimum all `kithara_audio::stream_source_tests`

Success criteria:
- all integration tests are green
- no hang-detector trips in seek lifecycle tests
- no decoder recreation on same-codec ABR switch
- no recovery/fallback branch is needed to make seek pass
- HLS seek state transitions are explainable from one authoritative post-seek position

## Implementation Notes

- The dedupe patch was useful as a diagnostic step because it proved queue flooding was secondary.
- It should not be treated as the final architecture.
- The refactor should aim to make that patch unnecessary, or absorb its valid part into a single explicit demand lifecycle instead of keeping it as compensating glue.
