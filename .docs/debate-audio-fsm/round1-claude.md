# Round 1 — Claude (Backend Architect) Analysis

## 1.1: Missing `Decoding -> WaitingForSource` Transition
Source may transition to `WaitingDemand` during normal playback. FSM only allows `WaitingForSource` from seek-related states.
**Fix:** Make `epoch`/`target` optional in `WaitingForSource`, or add `BlockedOnSource` state.

## 1.2: No Format Change Draining State
Between detection and recreation, old decoder still drains data. Neither `Decoding` nor `RecreatingDecoder` covers this.
**Fix:** Add `DrainingForFormatChange` state or document as Decoding sub-state.

## 1.3: Missing `AwaitingFirstChunk -> SeekRequested`
New seek can arrive while waiting for first post-seek chunk. No transition defined.
**Fix:** Add to state chart.

## 1.4: Missing `WaitingForSource -> AtEof`
Source can reach EOF while track waits. Track would be stuck.
**Fix:** Add `WaitingForSource -> AtEof` and `-> Failed` transitions.

## 1.5: Missing `RecreatingDecoder -> WaitingForSource`
Init segment bytes may not be available. Transient unavailability treated as permanent failure.
**Fix:** Add transition or document internal retry.

## 2.1: TrackStatus Synchronization Unspecified
Not atomically updatable. Mutex contention on hot path or unspecified lock-free mechanism.
**Fix:** AtomicU8 tag for coarse state + Mutex for full snapshot.

## 2.2: Dual-Truth Timeline vs TrackState During Migration
Both claim seek authority. Inconsistent views possible.
**Fix:** Define priority rule: Timeline remains authority for Phase 1.

## 2.3: SeekRequested Epoch Split-Brain
FSM latches epoch but Audio::seek() writes to Timeline. Second seek can diverge.
**Fix:** SeekRequested should re-read from Timeline on entry, not latch.

## 3.1: Phase 2 Highest Risk — pending_fetch Ownership Unclear
Where does `pending_fetch` live after `step_track` replaces worker methods?
**Fix:** Clarify pending_fetch stays in TrackSlot.

## 3.2: Seek Path Rewritten Twice (Phase 1 + Phase 2)
Doubles regression surface.
**Fix:** Merge Phase 1 and 2 for seek path, or make Phase 1 passive observer.

## 3.3: ConsumerPhase Ignores Preload Contract
`preloaded` flag (WASM path) not mentioned. Could break non-blocking mode.
**Fix:** Document preloaded as data-plane flag orthogonal to ConsumerPhase.

## 3.4: No Rollback Plan Per Phase
No mechanism to revert Phase 2 without reverting Phase 1.
**Fix:** Keep old methods behind `#[deprecated]` during transition.

## 4.1-4.5: Testing Gaps
- No WaitingForSource timeout/escalation test
- No superseding seek during RecreatingDecoder test
- No test verifying silent-abandon removal
- No test for step_track -> Blocked with correct WaitingReason
- No property-based FSM traversal test

## 5.1: step_track Conflates FSM Advance with Output Production
Multiple transitions needed before producing output. Cooperative scheduling at risk.
**Fix:** Single FSM transition per call. StateChanged counts as Progress.

## 5.2: TrackStep::Idle Ambiguous
Overlaps with Blocked, AtEof, Failed.
**Fix:** Remove Idle.

## 5.3: TrackState Exposes Too Much to Worker
Worker gets epoch, target, attempt, cause, offset — leaks seek detail.
**Fix:** Use `TrackPhaseTag` (fieldless discriminant) for worker, full state for diagnostics.

## 5.4: DecoderSession Construction Ownership Unclear
Factory vs FSM constructing DecoderSession.
**Fix:** Factory returns `Option<DecoderSession>` atomically.

## 6.1: TrackFailure::InvalidState is Code Smell
`&'static str` not queryable or matchable.
**Fix:** `InvalidTransition { from: TrackPhaseTag, trigger: &'static str }`.

## 6.2: WASM Path Not Addressed
ConsumerPhase must work in non-blocking mode.
**Fix:** Note in Phase 3 about WASM validation.

## 6.3: No Tracing Spec for FSM Transitions
Primary motivation is diagnosability but no tracing events specified.
**Fix:** Every transition emits `debug!(from, to, trigger)`. Terminal states at `warn!`.
