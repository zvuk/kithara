# Debate Synthesis: Audio Track FSM Design Review

**Date**: 2026-03-12
**Participants**: Gemini, Codex (GPT), Claude
**Subject**: `.docs/2026-03-12-kithara-audio-track-fsm-design.md`
**Rounds**: 2

---

## Executive Summary

All three agents agree the design is sound in its core insight: hidden `pending_*` fields in `StreamAudioSource` constitute an implicit FSM and must be made explicit. The ownership model (Section 5) is the strongest part of the document.

However, the design has **critical gaps** that must be fixed before implementation. The top issue — missing `Decoding -> WaitingForSource` transition — is the exact failure mode the FSM was designed to fix.

---

## Consensus Issues (All Three Agents Agree)

### C1. Missing `Decoding -> WaitingForSource` Transition [CRITICAL]

**The #1 priority fix.** The design's motivating bug (Section 2.3, `test6.log` hang) is: source enters `WaitingDemand` during normal playback, but `kithara-audio` reduces this to `is_ready() == false`. The proposed FSM has no path from `Decoding` to `WaitingForSource` — it can only represent source-blocked states during seeks.

**Fix:** Add `Decoding -> WaitingForSource { epoch: None, target: None, reason, attempt: 0 }`. Make `epoch`/`target` optional in `WaitingForSource` to support both seek-initiated and playback-initiated waits. Add reverse transition `WaitingForSource -> Decoding` when source becomes ready again.

### C2. Superseding Seek Must Be a Universal Preemption Rule [CRITICAL]

The state chart only shows `WaitingForSource -> SeekRequested` for superseding seeks. But a new seek can arrive in any non-terminal state: `ApplyingSeek`, `RecreatingDecoder`, `AwaitingFirstChunk`, `Decoding`, `WaitingForSource`.

**Fix:** Add a global rule: "For every non-terminal state, if `timeline.seek_epoch() > current_epoch`, transition to `SeekRequested` with the new epoch." Check this at the top of `step_track`, before state-specific logic.

### C3. `TrackStatus` Lost-Wakeup Race [HIGH]

Shared `TrackStatus` with "worker wakes consumer on state transition" has a classic lost-wakeup race. Consumer can observe stale status, miss the wake signal, and park forever.

**Fix:** Specify the synchronization protocol explicitly:
1. Worker updates `TrackStatus` (under lock or with atomic sequence counter)
2. Worker signals wake
3. Consumer: check TrackStatus → if not terminal and no data, register wait → re-check TrackStatus → only then park

### C4. Silent Seek-Abandon Path Must Be Removed [HIGH]

Current code (`source.rs:1242-1255`) silently continues playback after `MAX_SEEK_RETRY` failures. The design forbids this (Section 8.5) but doesn't specify the replacement behavior.

**Fix:** Emit `SeekRejected { epoch, target, attempts }` event. Transition to `Decoding` (not `Failed`) — the track is still functional, it just didn't honor the seek. The consumer and diagnostics can observe the rejection.

### C5. Dual-Truth During Migration (Timeline vs TrackState) [HIGH]

All three agents agree this is the migration's Achilles' heel. During Phases 1-2, both `Timeline` and `TrackState` claim authority over seek lifecycle.

**Fix:** Define an explicit authority matrix:

| Concern | Phase 1 | Phase 2 | Phase 3 |
|---|---|---|---|
| Seek epoch | Timeline | Timeline (read by step_track) | Timeline |
| Seek completion | Timeline | TrackState | TrackState |
| Source readiness | TrackSlot.is_ready() | step_track Blocked | step_track |
| Consumer seek state | pending_seek_epoch | pending_seek_epoch | ConsumerPhase |

Phase 1 FSM runs in **shadow mode** (asserts parity, never drives behavior).

---

## Consensus Issues (Two of Three Agents Agree)

### C6. Incomplete `SourcePhase` → `TrackState` Mapping [HIGH]
*Flagged by: Codex, Claude*

`WaitingReason` only covers transient waits (`Waiting`, `WaitingDemand`, `WaitingMetadata`). But `SourcePhase` also includes `Cancelled`, `Stopped`, `Eof`, `Seeking` — these have no defined mapping to `TrackState`/`TrackFailure`.

**Fix:** Add a normative mapping table:
- `SourcePhase::Cancelled` → `Failed(TrackFailure::SourceCancelled)`
- `SourcePhase::Stopped` → `Failed(TrackFailure::SourceStopped)` or `AtEof` depending on context
- `SourcePhase::Eof` → `AtEof`
- `SourcePhase::Seeking` → no-op (FSM already in a seek state)

### C7. Missing State Chart Transitions [MEDIUM]
*Flagged by: Claude, partially Gemini*

Several transitions missing from the chart:
- `AwaitingFirstChunk -> SeekRequested` (superseding seek, covered by C2)
- `WaitingForSource -> AtEof` (source reaches EOF while waiting)
- `WaitingForSource -> Failed` (source cancelled while waiting)
- `RecreatingDecoder -> WaitingForSource` (init segment bytes unavailable)

### C8. Retry Semantics Underdefined [MEDIUM]
*Flagged by: Codex, Claude*

`attempt: u8` fields scattered across states without carry/reset rules. Current code has distinct retry behaviors for outer seek application and post-seek decode recovery.

**Fix:** Document carry/reset rules:
- `SeekRequested` resets all counters
- `WaitingForSource` preserves the counter from its parent state
- `ApplyingSeek` increments on each re-entry from `WaitingForSource`
- Superseding seek resets everything

---

## Resolved Disagreements

### D1. `step_track` Execution Model: Looping vs Single-Step

| Agent | Position |
|---|---|
| Gemini | Internal loop through transient transitions |
| Codex | Single transition per call |
| Claude | Single transition per call |

**Resolution: Single transition per call.** Internal looping violates cooperative scheduling and can starve other tracks. `StateChanged` counts as `Progress` — worker yields and re-enters round-robin. Gemini's concern about "empty ticks" is mitigated by treating `StateChanged` as progress.

### D2. `Closed`/`Shutdown` State

| Agent | Position |
|---|---|
| Gemini | Add `Closed` terminal state |
| Codex | Keep cancellation out of TrackState |
| Claude | Do not add `Closed` — CancellationToken is orthogonal |

**Resolution: No `Closed` state.** Track removal is driven by `CancellationToken` in `TrackSlot::is_removable()`. The FSM models track control lifecycle, not ownership lifecycle.

### D3. Splitting `RecreatingDecoder` by Cause

| Agent | Position |
|---|---|
| Codex | Split into separate states or carry optional seek context |
| Claude | Keep single state with `RecreateCause` enum |
| Gemini | Did not address specifically |

**Resolution: Keep single `RecreatingDecoder` with `RecreateCause`.** The cause enum already discriminates. However, rename `AwaitingFirstChunk` to `AwaitingResume` and make seek context (`epoch`, `skip`) optional to support format-boundary recreation without seek context.

### D4. `SeekRequested` Should Latch vs Re-read from Timeline

| Agent | Position |
|---|---|
| Claude R1 | Re-read from Timeline on entry |
| Codex R2 | Latch (snapshot) once, use global supersession rule |

**Resolution: Latch (snapshot) once, with supersession.** Codex is correct that re-reading defeats the purpose of moving control out of Timeline. The FSM snapshots `{epoch, target}` when entering `SeekRequested`. The universal preemption rule (C2) handles newer epochs arriving later.

### D5. Phase 1 Migration Strategy

| Agent | Position |
|---|---|
| Gemini | Shim layer in Phase 0.5 |
| Claude | Shadow mode (asserts, doesn't drive) |
| Codex | Keep staged, add authority matrix |

**Resolution: Shadow mode + authority matrix.** Phase 1 FSM runs alongside existing fields, asserting parity. No shim. Authority matrix documents who drives behavior at each phase.

---

## New Insights from Round 2 (Not in Round 1)

### N1. `pending_fetch` Stale-Epoch Contamination
*Discovered by: Claude R2*

When a seek arrives, the worker must clear `pending_fetch`. Under single-transition `step_track`, there's a window where `step_track` returns `StateChanged(SeekRequested)` but the scheduler hasn't cleared `pending_fetch` yet. Next call could push stale data.

**Fix:** Add to design: "Entering `SeekRequested` state must trigger `pending_fetch = None` in the scheduler slot."

### N2. `DecodeError::Interrupted` Semantics Change
*Discovered by: Claude R2*

Currently, the decode loop checks `timeline.is_flushing()` to escape Symphonia's blocking `read()` calls. Under FSM-driven control, the decode loop should not read Timeline directly. But removing the check means Symphonia can block for hundreds of ms.

**Fix:** The FSM sets an `AtomicBool` interrupt flag when transitioning to `SeekRequested`. The `Read` impl checks this flag. Add as Phase 2 requirement.

### N3. Effects Chain Reset Timing
*Discovered by: Claude R2, Codex R2*

Effects reset/flush not specified in FSM transitions. Stale reverb/EQ tails can bleed across seek points.

**Fix:** Add to design: "Effects reset on entry to `SeekRequested` and `RecreatingDecoder`. Effects flush on entry to `AtEof`."

### N4. `DecoderSession` Must Be Atomically Constructed
*Discovered by: Claude R2*

Current code (`source.rs:475-476`) updates `cached_media_info` and `base_offset` before checking factory success. If factory fails, old decoder uses stale metadata.

**Fix:** `DecoderSession` is replaced only on successful factory return. No partial updates.

### N5. Hang Detector Integration
*Discovered by: Claude R2*

Current hang detection (`hang_tick!/hang_reset!`) is not addressed in FSM design. Under `step_track`, the per-decode-chunk `hang_tick!()` inside the decode loop would be lost.

**Fix:** Worker-level `hang_tick!()` on non-terminal non-progress. Decode loop retains its own `hang_tick!()` for intra-step stall detection.

### N6. TrackStatus Must Publish on SourcePhase Changes
*Discovered by: Codex R2*

`WaitingMetadata -> WaitingDemand -> Waiting` can all happen while TrackState remains "blocked." Design only requires updates "on every transition."

**Fix:** TrackStatus must also update when `source_phase` changes, not just on FSM state transitions.

### N7. Failure Must Outrank EOF in Delivery
*Discovered by: Codex R2*

Worker panic path sets `Failed`, then pushes EOF fetch. Consumer can observe "natural EOF" before "failed."

**Fix:** Explicit terminal precedence: `Failed` always takes priority over `AtEof` in consumer observation.

### N8. Seek Position Semantics Inconsistency
*Discovered by: Codex R2*

`Timeline::initiate_seek()` updates committed position immediately, but `OutputCommitted`/`SeekComplete` happen later. Position jumps optimistically.

**Fix:** Split `requested_position` from `output_committed_position`, or explicitly document optimistic position jump semantics.

### N9. WASM Path Not Addressed
*Discovered by: Claude R1*

`ConsumerPhase` must work in non-blocking mode. `preloaded` flag gates `use_nonblocking_recv()` on WASM. Not mentioned in migration plan.

**Fix:** Document `preloaded` as data-plane flag orthogonal to `ConsumerPhase`. Add WASM integration tests as Phase 3 validation gate.

---

## Recommended Design Amendments

### Priority 1 (Must fix before implementation)

1. Add `Decoding -> WaitingForSource` with optional epoch/target
2. Add universal `* -> SeekRequested` preemption rule
3. Add `SourcePhase` → `TrackState` normative mapping table
4. Add missing transitions: `WaitingForSource -> AtEof/Failed`, `RecreatingDecoder -> WaitingForSource`, `AwaitingFirstChunk -> SeekRequested`
5. Define authority matrix for each migration phase
6. Specify `TrackStatus` synchronization protocol (check-register-recheck)

### Priority 2 (Should fix before implementation)

7. Specify retry budget carry/reset rules
8. Rename `AwaitingFirstChunk` → `AwaitingResume` with optional seek context
9. Remove `TrackStep::Idle` (replace with `Blocked`)
10. Use `TrackPhaseTag` (fieldless discriminant) for worker, full `TrackState` for diagnostics
11. Specify effects chain reset/flush timing per transition
12. Specify `DecoderSession` atomic construction
13. Add `pending_fetch` clearing requirement on seek entry

### Priority 3 (Address during implementation)

14. Add `AtomicBool` interrupt flag for decode-loop escape under FSM
15. Hang detector integration specification
16. TrackStatus updates on SourcePhase changes
17. Failure-outranks-EOF terminal precedence
18. WASM non-blocking mode compatibility
19. Silent seek-abandon replacement: `SeekRejected` event + resume at current position
20. Structured tracing for every FSM transition

### Required New Tests

1. `Decoding -> WaitingForSource(WaitingDemand) -> Decoding` (source starvation/recovery)
2. Superseding seek during `RecreatingDecoder`, `ApplyingSeek`, `AwaitingFirstChunk`
3. Silent-abandon removal: 3 seek failures → `SeekRejected` event (not silent resume)
4. `step_track` returns `Blocked(WaitingDemand)` when source is in `WaitingDemand`
5. `RecreatingDecoder -> Failed` (factory returns None)
6. `WaitingForSource -> AtEof` (source EOF during wait)
7. State-only wakeup with empty PCM queue (lost-wakeup regression)
8. Old-epoch events never emitted after superseding seek
9. Property-based random FSM traversal (all transitions in allowed set)
10. WASM non-blocking mode with ConsumerPhase transitions

---

## Revised State Chart

```text
Decoding
  -> SeekRequested        (new seek observed)
  -> WaitingForSource     (source not ready during playback) [NEW]
  -> RecreatingDecoder    (format change detected)
  -> AtEof                (decoder EOF)
  -> Failed               (unrecoverable decode error)

SeekRequested
  -> WaitingForSource     (source not ready for seek)
  -> ApplyingSeek         (source ready, applying seek)
  -> RecreatingDecoder    (seek requires decoder recreation)
  -> SeekRequested        (superseding seek — re-latch) [implicit via C2]
  -> Failed               (invalid state)

WaitingForSource
  -> ApplyingSeek         (source ready, in seek context)
  -> Decoding             (source ready, no seek context) [NEW]
  -> RecreatingDecoder    (source ready, need recreation)
  -> SeekRequested        (superseding seek)
  -> AtEof                (source EOF during wait) [NEW]
  -> Failed               (source cancelled/stopped) [NEW]

ApplyingSeek
  -> AwaitingResume       (seek applied successfully)
  -> WaitingForSource     (source not ready mid-apply)
  -> RecreatingDecoder    (seek requires decoder recreation)
  -> SeekRequested        (superseding seek) [NEW]
  -> Failed               (seek exhausted retries → SeekRejected event, or fatal)

RecreatingDecoder
  -> ApplyingSeek         (recreation done, apply seek)
  -> AwaitingResume       (recreation done, no seek context)
  -> WaitingForSource     (init segment unavailable) [NEW]
  -> SeekRequested        (superseding seek) [NEW]
  -> Failed               (factory failed)

AwaitingResume (renamed from AwaitingFirstChunk)
  -> Decoding             (first valid chunk produced)
  -> RecreatingDecoder    (first chunk fails)
  -> SeekRequested        (superseding seek) [NEW]
  -> Failed               (decode failure)

AtEof
  -> SeekRequested        (user seeks from EOF)

Failed
  -> terminal
```

---

## Cost Summary

| Agent | Rounds | Calls |
|---|---|---|
| Gemini | 2 | 2 |
| Codex | 2 | 2 |
| Claude (subagent) | 2 | 2 |
