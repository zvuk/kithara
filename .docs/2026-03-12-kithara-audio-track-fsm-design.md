# Kithara Audio Track FSM Design

**Date**: 2026-03-12
**Status**: proposed design
**Scope**: `crates/kithara-audio`

## 1. Purpose

This document defines the refactor of `kithara-audio` to an explicit
track-level FSM.

`kithara-stream` is already on a shared source-phase FSM:
- `SourcePhase` in `crates/kithara-stream/src/source.rs`
- `FileSource` adapted in `crates/kithara-file/src/session.rs`
- `HlsSource` adapted in `crates/kithara-hls/src/source.rs`

That work is not being redone here.

The problem that remains is above the stream layer:
- `StreamAudioSource` owns a hidden seek/recreate/recovery machine
- `AudioWorker` encodes part of that machine in `TrackPhase::PendingReset`
- `Audio` consumer observes only "chunk arrived / no chunk / closed"
- source wait reasons such as `WaitingDemand` are lost before they reach the
  audio layer

This document makes the `kithara-audio` track lifecycle explicit and defines
which current fields, methods, and control patterns must be removed.

## 2. Problem Statement

### 2.1 What is explicit today

Only the shared worker scheduler has an explicit FSM:
- `TrackPhase::{Decoding, PendingReset, AtEof, Failed}` in
  `crates/kithara-audio/src/pipeline/worker_types.rs`

This FSM is too coarse. `PendingReset` currently covers several distinct
control situations:
- seek observed
- waiting for source readiness
- applying seek plan
- recreating decoder
- retrying a failed post-seek decode

### 2.2 What is implicit today

The real control state of a track is spread across:

`StreamAudioSource` fields:
- `pending_format_change`
- `pending_decode_started_epoch`
- `pending_seek_skip`
- `pending_seek_recover_target`
- `pending_seek_recover_attempts`
- `seek_retry_count`
- `cached_media_info`
- `base_offset`
- `decoder`

`Audio` fields and timeline usage:
- `eof`
- `preloaded`
- `validator.epoch`
- `timeline.pending_seek_epoch()`

`Timeline` coordination flags:
- `flushing`
- `seek_pending`
- `seek_epoch`
- `seek_target`
- `pending_seek_epoch`

### 2.3 Why this causes hangs

`HlsSource` can already classify why it is blocked:
- `Waiting`
- `WaitingDemand`
- `WaitingMetadata`

But `kithara-audio` reduces that to:
- `is_ready() == false`
- PCM channel is empty

The result is the failure shape seen in `test6.log`:
- source is in `WaitingDemand`
- worker reports only `NoProgress`
- consumer blocks in `recv_outcome_blocking()`
- hang detector fires without the audio layer having an explicit control state

## 3. Design Goals

The refactor must satisfy all of these:

1. `kithara-stream` remains the source-layer FSM.
2. `kithara-audio` gains an explicit track-control FSM.
3. Source wait reasons must survive into the audio layer.
4. Silent fallback chains must be replaced with explicit transitions.
5. The scheduler must stop owning seek/reset semantics.
6. Public `Audio<Stream<T>>` behavior should remain stable during phase 1.
7. Existing seek regression tests remain the primary contract.

## 4. Non-goals

This document does not propose:
- a new `kithara-stream` FSM
- a downloader actor rewrite
- removing `Timeline` in the first phase
- changing the public `Audio` API in the first phase

The design is intentionally incremental.

## 5. Ownership Model After Refactor

The refactor is based on clear ownership.

### 5.1 `kithara-stream::SourcePhase`

Owner:
- source/download layer

Responsibility:
- classify source readiness
- explain why bytes are not yet readable

Not responsible for:
- decoder lifecycle
- seek recovery policy
- consumer buffering state

### 5.2 Track FSM in `kithara-audio`

Owner:
- `StreamAudioSource`

Responsibility:
- seek lifecycle
- decoder recreation lifecycle
- post-seek recovery
- waiting-on-source state
- terminal track state

This is the missing explicit FSM.

### 5.3 Worker scheduler

Owner:
- `AudioWorker`

Responsibility:
- round-robin scheduling
- backpressure resend via `pending_fetch`
- cancellation / removal

Not responsible for:
- deciding whether a track is currently seeking
- deciding which fallback path to try
- interpreting source phases

### 5.4 Consumer

Owner:
- `Audio`

Responsibility:
- chunk consumption
- output commit
- buffering / playing / eof / failed consumer-facing phase

Not responsible for:
- guessing track control state from an empty PCM channel

### 5.5 `Timeline`

Owner:
- low-level shared synchronization

Responsibility in phase 1:
- hold seek target and epoch
- hold committed position / duration / byte position
- expose flush/pending bits as low-level coordination primitives

Not responsible after refactor:
- being the owner of the track lifecycle

## 6. Target Track FSM

The new FSM is per track and lives inside `StreamAudioSource`.

### 6.1 New core types

New file:
- `crates/kithara-audio/src/pipeline/track_fsm.rs`

Required types:

```rust
pub(crate) enum TrackState {
    Decoding,
    SeekRequested {
        epoch: u64,
        target: Duration,
    },
    WaitingForSource {
        epoch: u64,
        target: Duration,
        reason: WaitingReason,
        attempt: u8,
    },
    ApplyingSeek {
        epoch: u64,
        target: Duration,
        mode: SeekMode,
        attempt: u8,
    },
    RecreatingDecoder {
        epoch: u64,
        target: Duration,
        cause: RecreateCause,
        offset: u64,
        attempt: u8,
    },
    AwaitingFirstChunk {
        epoch: u64,
        skip: Option<Duration>,
    },
    AtEof,
    Failed(TrackFailure),
}
```

```rust
pub(crate) enum WaitingReason {
    Waiting,
    WaitingDemand,
    WaitingMetadata,
}
```

```rust
pub(crate) enum SeekMode {
    Direct,
    Anchor(SourceSeekAnchor),
}
```

```rust
pub(crate) enum RecreateCause {
    FormatBoundary,
    SeekRecovery,
    CodecChange,
}
```

```rust
pub(crate) struct DecoderSession {
    pub base_offset: u64,
    pub decoder: Box<dyn InnerDecoder>,
    pub media_info: Option<MediaInfo>,
}
```

### 6.2 State chart

```text
Decoding
  -> SeekRequested
  -> RecreatingDecoder
  -> AtEof
  -> Failed

SeekRequested
  -> WaitingForSource
  -> ApplyingSeek
  -> RecreatingDecoder
  -> Failed

WaitingForSource
  -> ApplyingSeek
  -> RecreatingDecoder
  -> SeekRequested        (superseding seek)
  -> Failed

ApplyingSeek
  -> AwaitingFirstChunk
  -> WaitingForSource
  -> RecreatingDecoder
  -> Failed

RecreatingDecoder
  -> ApplyingSeek
  -> AwaitingFirstChunk
  -> Failed

AwaitingFirstChunk
  -> Decoding
  -> RecreatingDecoder
  -> Failed

AtEof
  -> SeekRequested

Failed
  -> terminal
```

## 7. Required Semantics Per State

### 7.1 `Decoding`

Allowed actions:
- decode next chunk
- detect format change
- detect new seek request

Forbidden:
- keeping hidden post-seek recovery flags outside the state

### 7.2 `SeekRequested`

Meaning:
- a new seek epoch was observed
- target is latched
- old per-seek state has been invalidated

Required actions:
- clear stale per-seek recovery state
- resolve anchor or direct-seek path
- inspect current source phase

### 7.3 `WaitingForSource`

Meaning:
- the next control action cannot proceed because source bytes are not ready

Required actions:
- store exact reason from `SourcePhase`
- store current seek epoch and target
- wake consumer and worker listeners on state change

Forbidden:
- collapsing this to `is_ready() == false`

### 7.4 `ApplyingSeek`

Meaning:
- the track is attempting a concrete seek plan

Required actions:
- perform direct seek or anchor-based seek
- localize retry budget to this state

Forbidden:
- storing retry counters as free fields on `StreamAudioSource`

### 7.5 `RecreatingDecoder`

Meaning:
- the current decoder session is not valid for the next read path

Required actions:
- hold recreate cause explicitly
- hold recreate offset explicitly
- rebuild `DecoderSession`

Forbidden:
- using `pending_format_change` as an out-of-band signal

### 7.6 `AwaitingFirstChunk`

Meaning:
- seek has been applied
- the first valid chunk of the new epoch has not yet been produced

Required actions:
- own post-seek skip duration
- own the `DecodeStarted` emission edge

Forbidden:
- tracking this via `pending_decode_started_epoch`

### 7.7 `AtEof`

Meaning:
- the decoder session is at true EOF for the current layout

Allowed transitions:
- only a new seek can revive the track

### 7.8 `Failed`

Meaning:
- unrecoverable control or decode failure

Required:
- exact failure cause is stored in state
- consumer is woken immediately

Forbidden:
- silently continuing playback after retry exhaustion without an explicit
  failure or rejection event

## 8. Structures and Fields To Remove

This section is normative.

### 8.1 `StreamAudioSource`

File:
- `crates/kithara-audio/src/pipeline/source.rs`

The following fields must be deleted from `StreamAudioSource` and replaced by
FSM-owned state:

Delete:
- `pending_format_change`
- `pending_decode_started_epoch`
- `pending_seek_skip`
- `pending_seek_recover_target`
- `pending_seek_recover_attempts`
- `seek_retry_count`

Replace:
- `decoder`
- `cached_media_info`
- `base_offset`

With:
- `DecoderSession`
- `TrackState`

Rationale:
- these fields are not independent data; together they already describe the
  hidden state machine

### 8.2 `AudioWorkerSource` trait

File:
- `crates/kithara-audio/src/pipeline/worker.rs`

Delete these methods:
- `fetch_next`
- `apply_pending_seek`
- `is_ready`

Replace them with one control entrypoint:

```rust
fn step_track(&mut self) -> TrackStep<Self::Chunk>;
```

Required return type:

```rust
pub(crate) enum TrackStep<C> {
    Produced(Fetch<C>),
    Blocked(WaitingReason),
    StateChanged(TrackState),
    AtEof,
    Failed(TrackFailure),
    Idle,
}
```

`timeline()` may remain temporarily, but it must stop being used by the worker
as the source of track control decisions.

### 8.3 Worker phase model

Files:
- `crates/kithara-audio/src/pipeline/worker_types.rs`
- `crates/kithara-audio/src/pipeline/audio_worker.rs`

Delete:
- `TrackPhase::PendingReset`

Target end state:
- `AudioWorker` no longer models seek/reset/recreate as worker phases
- scheduler behavior is derived from `TrackStep`

Preferred end state:
- delete `TrackPhase` entirely

Acceptable intermediate state:
- keep a reduced scheduler-only phase enum, but it must not encode seek logic

### 8.4 `Audio` consumer-side hidden seek state

File:
- `crates/kithara-audio/src/pipeline/audio.rs`

Delete as control-state owners:
- `eof`
- `timeline.pending_seek_epoch()` as the source of seek-completion truth

Replace with:

```rust
pub(crate) enum ConsumerPhase {
    Buffering,
    Playing,
    SeekPending { epoch: u64 },
    AtEof,
    Failed,
}
```

Keep:
- `current_chunk`
- `chunk_offset`

These are data-plane state, not control-plane state.

### 8.5 Silent retry-abandon path

File:
- `crates/kithara-audio/src/pipeline/source.rs`

Delete the behavior:
- "seek abandoned after max retries, continuing from current position"

Retry exhaustion must become an explicit transition:
- either `Failed(TrackFailure::SeekApplyExhausted)`
- or an explicit `SeekRejected` event followed by deterministic transition

Silent continuation is forbidden.

## 9. New Supporting Types

### 9.1 `TrackFailure`

New file:
- `crates/kithara-audio/src/pipeline/track_fsm.rs`

```rust
pub(crate) enum TrackFailure {
    Decode(DecodeError),
    SeekApplyExhausted { epoch: u64, target: Duration },
    DecoderRecreateFailed { epoch: u64, offset: u64 },
    InvalidState(&'static str),
}
```

### 9.2 `TrackStatus`

New file:
- `crates/kithara-audio/src/pipeline/track_fsm.rs`

This is a lightweight externally-visible snapshot of the FSM for consumer and
diagnostics.

```rust
pub(crate) struct TrackStatus {
    pub state: TrackState,
    pub source_phase: Option<SourcePhase>,
}
```

This status must be updated on every transition and made visible to `Audio`.

## 10. Worker / Consumer Contract

The worker must stop exposing only "chunk or nothing".

### 10.1 Phase 1 contract

Keep the PCM channel, but add explicit shared `TrackStatus`.

Required:
- worker wakes consumer on both chunk arrival and control-state transition
- `recv_outcome_blocking()` consults `TrackStatus` before parking again
- logs and diagnostics use `TrackStatus` rather than blind empty-channel loops

### 10.2 Phase 2 contract

Optional later cleanup:
- replace shared `TrackStatus` with a typed control event stream if needed

This is not required for phase 1.

## 11. Event Model

Existing audio events remain, but ownership changes.

### 11.1 Event edges that must be produced by FSM

`SeekLifecycle::SeekRequest`
- when entering `SeekRequested`

`SeekLifecycle::SeekApplied`
- when entering `AwaitingFirstChunk`

`SeekLifecycle::DecodeStarted`
- when leaving `AwaitingFirstChunk` on the first valid chunk

### 11.2 Event edge that remains consumer-owned

`SeekLifecycle::OutputCommitted`
- emitted when `Audio` in `ConsumerPhase::SeekPending { epoch }` writes the
  first matching-epoch sample to output

`SeekComplete`
- emitted on `ConsumerPhase::SeekPending -> Playing`

## 12. Migration Plan

### Phase 0: Introduce types and state tests

Add:
- `track_fsm.rs`
- unit tests for transitions

No behavior change yet.

### Phase 1: Move `StreamAudioSource` hidden fields into `TrackState`

Required cleanup:
- replace hidden `pending_*` fields
- replace `decoder + cached_media_info + base_offset` with `DecoderSession`

Compatibility allowed:
- `Timeline` seek bits stay in place
- public `Audio` API unchanged

### Phase 2: Replace `fetch_next/is_ready/apply_pending_seek` with `step_track`

Required:
- worker delegates control to the source FSM
- worker stops interpreting `timeline.is_flushing()` directly

### Phase 3: Introduce `ConsumerPhase` and remove `pending_seek_epoch` from
consumer control

Required:
- `Audio` stops inferring seek completion from timeline pending flag
- `Audio` uses explicit consumer phase

### Phase 4: Delete compatibility shims

Delete:
- any remaining `PendingReset`
- any remaining free retry counters
- any remaining duplicated seek lifecycle flags

## 13. Required Test Strategy

Existing tests that remain authoritative:
- `seek_during_active_decode_completes_without_hang`
- `rapid_seeks_via_timeline_all_complete`
- `seek_during_pending_format_change_retries_on_new_decoder`
- repeated-seek-after-eof recovery cases
- seek-recovery target stability cases

New required tests:
- track FSM transition tests for each non-terminal state
- `WaitingDemand` is reflected in `TrackStatus`
- consumer does not blind-wait when `TrackState` is terminal
- retry exhaustion is explicit, never silent
- `AwaitingFirstChunk` owns `DecodeStarted` edge

## 14. Success Criteria

The refactor is complete only when all of the following are true:

1. `kithara-stream` remains the only source-layer FSM.
2. `kithara-audio` has one explicit track-control FSM.
3. `StreamAudioSource` no longer has free-floating `pending_*` seek flags.
4. Worker does not own seek/reset semantics.
5. Consumer does not infer control state from an empty PCM queue.
6. Source wait reasons survive into the audio layer.
7. Retry exhaustion is explicit.
8. `test6.log`-style hangs become diagnosable by state, not only by watchdog.

## 15. Decision

The required refactor target is:

- keep `SourcePhase` in `kithara-stream`
- add explicit track FSM in `kithara-audio`
- make worker a scheduler only
- make consumer state explicit
- remove hidden seek/recovery fields and silent fallback behavior

This is the design baseline for all further code changes in this area.
