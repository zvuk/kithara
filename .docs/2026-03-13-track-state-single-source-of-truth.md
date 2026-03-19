# TrackState Single Source Of Truth Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove `StreamAudioSource` overlay state (`pending_*` fields and sibling retry counters) so seek, resume, format-boundary recovery, and decoder recreation are represented only by `TrackState`.

**Architecture:** Keep `DecoderSession` as runtime payload (`decoder + base_offset + media_info`), but move every transitional concern into explicit FSM payloads in `track_fsm.rs`. `source.rs` should stop storing hidden seek/recover/format flags and instead transition through real `SeekRequested`, `ApplyingSeek`, `AwaitingResume`, and `RecreatingDecoder` states. The public behavior stays the same: consumer-facing epoch filtering, seek lifecycle events, ABR decoder recreation, and Symphonia integration remain intact.

**Tech Stack:** Rust 2024, `kithara-audio`, `kithara-stream`, `kithara-decode`, Symphonia, HLS variant fence, `cargo test`, `cargo clippy`.

---

### Task 1: Lock The Current Contract With Tests

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/mod.rs`
- Create: `crates/kithara-audio/src/pipeline/source_state_tests.rs`
- Reference: `crates/kithara-audio/src/internal.rs`
- Reference: `crates/kithara-audio/src/pipeline/source.rs`
- Reference: `crates/kithara-audio/src/pipeline/track_fsm.rs`

**Step 1: Add a dedicated state-focused test module**

Add a test-only module declaration in `crates/kithara-audio/src/pipeline/mod.rs`:

```rust
#[cfg(test)]
mod source_state_tests;
```

**Step 2: Write the first failing tests**

Create `crates/kithara-audio/src/pipeline/source_state_tests.rs` with characterization tests that assert externally visible behavior, not field layout:

```rust
#[kithara::test]
fn first_chunk_after_seek_transitions_awaiting_resume_to_decoding() {
    // Arrange a source in post-seek state.
    // Act by driving step_track() until a chunk is produced.
    // Assert:
    // - state changes from AwaitingResume to Decoding
    // - DecodeStarted is emitted once
    // - chunk epoch matches seek epoch
}

#[kithara::test]
fn decode_error_after_seek_retries_inside_state_machine() {
    // Arrange a decoder that fails once immediately after seek and then succeeds.
    // Assert no side-channel state is required to recover.
}

#[kithara::test]
fn format_boundary_recovery_uses_recreating_decoder_state() {
    // Arrange format change metadata and EOF/error at boundary.
    // Assert recovery proceeds through RecreatingDecoder instead of pending flags.
}
```

**Step 3: Run tests to verify they fail for the right reason**

Run:

```bash
cargo test -p kithara-audio source_state_tests -- --nocapture
```

Expected: FAIL because the current implementation still depends on hidden `pending_*` state and cannot satisfy the new state-based assertions.

**Step 4: Keep test helpers behavior-driven**

If the tests need internal access, extend `crates/kithara-audio/src/internal.rs` with state-centric helpers only:

```rust
pub fn set_track_state<T: StreamType>(source: &mut StreamAudioSource<T>, state: TrackState) {
    source.0.state = state;
}

pub fn track_state_debug<T: StreamType>(source: &StreamAudioSource<T>) -> &TrackState {
    &source.0.state
}
```

Do not add new helpers for `pending_*` fields.

**Step 5: Commit**

```bash
git add crates/kithara-audio/src/pipeline/mod.rs crates/kithara-audio/src/pipeline/source_state_tests.rs crates/kithara-audio/src/internal.rs
git commit -m "test: lock stream audio source state transitions"
```

### Task 2: Move All Transitional Payload Into TrackState

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/track_fsm.rs`
- Reference: `crates/kithara-audio/src/pipeline/source.rs`

**Step 1: Define explicit state payload types**

Add small payload structs in `crates/kithara-audio/src/pipeline/track_fsm.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeState {
    pub recover_attempts: u8,
    pub seek: SeekContext,
    pub skip: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormatBoundary {
    pub media_info: MediaInfo,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecreateState {
    pub attempt: u8,
    pub boundary: Option<FormatBoundary>,
    pub cause: RecreateCause,
    pub offset: u64,
    pub seek: Option<SeekContext>,
}
```

**Step 2: Make `TrackState` own all in-flight context**

Update `TrackState` so the payload carries what `source.rs` currently stores in side fields:

```rust
pub(crate) enum TrackState {
    Decoding,
    SeekRequested { attempt: u8, seek: SeekContext },
    WaitingForSource { context: WaitContext, reason: WaitingReason },
    ApplyingSeek { attempt: u8, mode: SeekMode, seek: SeekContext },
    RecreatingDecoder(RecreateState),
    AwaitingResume(ResumeState),
    AtEof,
    Failed(TrackFailure),
}
```

**Step 3: Update `WaitContext` to carry recreation context directly**

Replace the current anonymous recreation payload with the same `RecreateState`-shaped data, so waiting and resuming do not need extra fields in `StreamAudioSource`.

**Step 4: Update phase-tag/unit tests**

Run:

```bash
cargo test -p kithara-audio track_fsm -- --nocapture
```

Expected: PASS after adapting the `track_fsm.rs` tests to the new payloads.

**Step 5: Commit**

```bash
git add crates/kithara-audio/src/pipeline/track_fsm.rs
git commit -m "refactor: move stream transition payload into track state"
```

### Task 3: Eliminate Post-Seek Overlay State

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/source.rs`
- Modify: `crates/kithara-audio/src/internal.rs`
- Reference: `crates/kithara-audio/src/pipeline/track_fsm.rs`

**Step 1: Delete the side-channel fields from `StreamAudioSource`**

Remove these fields from `crates/kithara-audio/src/pipeline/source.rs`:

```rust
pub(crate) pending_decode_started_epoch: Option<u64>,
pending_seek_skip: Option<(u64, Duration)>,
pub(crate) pending_seek_recover_target: Option<(u64, Duration)>,
pub(crate) pending_seek_recover_attempts: u8,
seek_retry_count: u8,
```

Also remove their initialization from `StreamAudioSource::new`.

**Step 2: Store resume/recovery data in `TrackState::AwaitingResume`**

Rewrite `apply_seek_applied()` to produce only state:

```rust
fn apply_seek_applied(&mut self, epoch: u64, position: Duration, /* ... */) {
    reset_effects(&mut self.effects);
    self.emit_seek_lifecycle(/* SeekApplied */);
    self.state = TrackState::AwaitingResume(ResumeState {
        recover_attempts: 0,
        seek: SeekContext { epoch, target: position },
        skip: None,
    });
}
```

For anchor-based seek, update the state payload instead of writing `pending_seek_skip`.

**Step 3: Make post-seek decode logic read from the state**

Refactor `apply_seek_skip()`, `retry_decode_failure_after_seek()`, and the `DecodeStarted` emission path to inspect `self.state`:

```rust
fn resume_state(&self) -> Option<&ResumeState> { /* match self.state */ }
fn resume_state_mut(&mut self) -> Option<&mut ResumeState> { /* match self.state */ }
```

Rules:
- The first successful chunk while in `AwaitingResume` emits `DecodeStarted` and transitions to `Decoding`.
- Skip trimming uses `ResumeState.skip`.
- One-shot retry increments `ResumeState.recover_attempts`.
- A successful chunk clears the whole recovery story by leaving `AwaitingResume`.

**Step 4: Move seek retry count into the seek state**

`apply_seek_from_timeline()` must stop mutating a field on `self`. Retry bookkeeping belongs in `TrackState::SeekRequested { attempt, .. }` or `TrackState::ApplyingSeek { attempt, .. }`.

**Step 5: Remove field-based test helpers**

Delete `set_seek_recover_state()` from `crates/kithara-audio/src/internal.rs` and replace it with explicit state constructors if tests still need setup.

**Step 6: Run focused tests**

Run:

```bash
cargo test -p kithara-audio source_state_tests::first_chunk_after_seek_transitions_awaiting_resume_to_decoding -- --nocapture
cargo test -p kithara-audio source_state_tests::decode_error_after_seek_retries_inside_state_machine -- --nocapture
```

Expected: PASS.

**Step 7: Commit**

```bash
git add crates/kithara-audio/src/pipeline/source.rs crates/kithara-audio/src/internal.rs
git commit -m "refactor: remove post-seek overlay state from stream audio source"
```

### Task 4: Replace Pending Format Change With Explicit Recreate State

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/source.rs`
- Modify: `crates/kithara-audio/src/pipeline/track_fsm.rs`
- Reference: `crates/kithara-stream/src/source.rs`
- Reference: `crates/kithara-hls/src/source.rs`

**Step 1: Delete `pending_format_change`**

Remove this field from `StreamAudioSource`:

```rust
pub(crate) pending_format_change: Option<(MediaInfo, u64)>,
```

**Step 2: Turn format detection into a pure query**

Change `detect_format_change()` from mutating `self` into returning a typed request:

```rust
fn detect_format_change(&self) -> Option<FormatBoundary> {
    // compare current media info vs session media info
    // compute target init-bearing offset
}
```

**Step 3: Route EOF/error recovery through `RecreatingDecoder`**

`handle_decode_eof()` and `handle_decode_error()` should no longer call `apply_format_change()` directly. Instead they should transition into `TrackState::RecreatingDecoder(...)`, then let a real step method perform the recreation.

Target shape:

```rust
self.state = TrackState::RecreatingDecoder(RecreateState {
    attempt: 0,
    boundary: Some(boundary),
    cause: RecreateCause::FormatBoundary,
    offset: boundary.offset,
    seek: None,
});
return DecodeAction::Continue;
```

**Step 4: Rework boundary recovery helpers**

Delete or inline:
- `apply_format_change()`
- `try_recover_at_boundary()`

Replace them with one helper that builds `RecreateState`, and one helper that executes the actual decoder recreation from that state.

**Step 5: Add the failing test first, then make it pass**

Run:

```bash
cargo test -p kithara-audio source_state_tests::format_boundary_recovery_uses_recreating_decoder_state -- --nocapture
```

Expected before implementation: FAIL.

Expected after implementation: PASS.

**Step 6: Commit**

```bash
git add crates/kithara-audio/src/pipeline/source.rs crates/kithara-audio/src/pipeline/track_fsm.rs
git commit -m "refactor: move format boundary recovery into track state"
```

### Task 5: Make `ApplyingSeek` And `RecreatingDecoder` Real FSM States

**Files:**
- Modify: `crates/kithara-audio/src/pipeline/source.rs`
- Reference: `crates/kithara-audio/src/pipeline/audio_worker.rs`

**Step 1: Implement `step_applying_seek()`**

Move synchronous seek application logic out of `step_seek_requested()` and into a dedicated state step:

```rust
fn step_seek_requested(&mut self) -> TrackStep<PcmChunk> {
    // transition only
    self.state = TrackState::ApplyingSeek { attempt, mode, seek };
    TrackStep::StateChanged
}

fn step_applying_seek(&mut self) -> TrackStep<PcmChunk> {
    // resolve anchor/direct path
    // on success -> AwaitingResume
    // on retryable failure -> SeekRequested with incremented attempt
    // on exhausted failure -> Failed(SeekExhausted) or Decoding only if that is the intended contract
}
```

**Step 2: Implement `step_recreating_decoder()`**

This state must:
- wait for source readiness when needed,
- seek stream to recreation offset,
- recreate decoder,
- optionally re-seek decoder if recovery requires it,
- transition to `AwaitingResume` or `Decoding` on success,
- transition to `Failed(TrackFailure::RecreateFailed { .. })` on hard failure.

**Step 3: Remove the fake fallback arms from `step_track()`**

Delete this behavior in `step_track()`:

```rust
TrackPhaseTag::ApplyingSeek => {
    self.state = TrackState::Decoding;
    TrackStep::StateChanged
}
TrackPhaseTag::RecreatingDecoder => {
    self.state = TrackState::Decoding;
    TrackStep::StateChanged
}
```

Replace it with real dispatch to `step_applying_seek()` and `step_recreating_decoder()`.

**Step 4: Revisit failure semantics**

Do not silently translate hard decode/recreate misuse into EOF inside `source.rs`. If the intended contract is “terminal failure”, return `TrackStep::Failed` so the worker can preserve the distinction between EOF and real failure.

**Step 5: Run the FSM-focused test slice**

Run:

```bash
cargo test -p kithara-audio source_state_tests -- --nocapture
```

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/kithara-audio/src/pipeline/source.rs
git commit -m "refactor: make stream audio source fsm states executable"
```

### Task 6: Cleanup, Verification, And Documentation

**Files:**
- Modify: `crates/kithara-audio/README.md`
- Reference: `crates/kithara-audio/src/pipeline/source.rs`
- Reference: `crates/kithara-audio/src/pipeline/track_fsm.rs`

**Step 1: Remove dead compatibility code**

Delete any test helper, comment, or docstring that still talks about:
- overlay `pending_*` fields,
- “old fetch_next/apply_pending_seek pattern” if it references removed field setup,
- implicit recovery state outside `TrackState`.

**Step 2: Update the README architecture section**

Add a short paragraph to `crates/kithara-audio/README.md` saying that seek, boundary recovery, and decoder recreation are modeled by explicit worker-side `TrackState` variants, and that `StreamAudioSource` no longer keeps duplicate pending flags.

**Step 3: Run the full package verification**

Run:

```bash
cargo test -p kithara-audio
cargo clippy -p kithara-audio --tests -- -D warnings
cargo fmt --check
```

Expected: all PASS.

**Step 4: Run one workspace-level regression slice if `kithara-audio` tests touch stream/decode contracts**

Run:

```bash
cargo test -p kithara-decode
cargo test -p kithara-stream
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/kithara-audio/README.md
git commit -m "docs: describe track state as single source of truth"
```

### Implementation Notes

- Keep `DecoderSession` separate from `TrackState`. It is live decoder runtime data, not transitional control state.
- Do not introduce a new “manager” object. The simplification comes from moving existing control payload into typed FSM variants, not from adding another abstraction layer.
- Prefer tiny helper accessors in `source.rs` such as `resume_state_mut()` and `recreate_state()` over new unused utility types.
- If a transition needs data that is not in `TrackState`, either move that data into the state or prove it is immutable runtime context.
- Preserve the current event contract:
  - `SeekRequest`
  - `SeekApplied`
  - `DecodeStarted`
  - `SeekRejected`
  - `EndOfStream`
- Preserve HLS-specific rules:
  - variant fence still blocks cross-variant reads,
  - recreation still starts from init-bearing segment offsets,
  - seek anchor logic still owns time-to-byte mapping.

### Suggested Execution Order

1. Task 1
2. Task 2
3. Task 3
4. Task 4
5. Task 5
6. Task 6

