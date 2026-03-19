# Round 2 -- Claude (Backend Architect)

## Part 1: Disagreements with Gemini and Codex

### 1.1 Disagreement with Gemini #7: Missing `Closed`/`Shutdown` State

Gemini proposes adding a `Closed` terminal state for explicit resource release when the user stops a track before EOF.

I disagree. Looking at the actual worker code (`audio_worker.rs:183-185`), track removal is driven by `CancellationToken::is_cancelled()` in `is_removable()`. The worker drains the slot from its `tracks` vec and the `TrackSlot` is dropped, releasing all resources. There is no point at which the FSM needs to represent "closed" -- by the time cancellation is observed, the slot is removed on the next cleanup pass. Adding a `Closed` state would create a dead transition that nothing drives and nothing observes. The cancellation token is the correct mechanism here; it is external to the FSM and orthogonal to it.

**Resolution:** Do not add `Closed`. Document that cancellation is handled by `CancellationToken`, not by FSM state. The FSM models *track control lifecycle*, not *track ownership lifecycle*.

### 1.2 Disagreement with Gemini #4: Phase 1 "Flag Day" / Shim Layer

Gemini warns that replacing `fetch_next/is_ready/apply_pending_seek` with `step_track` in Phase 2 is a flag day, and proposes an intermediate shim.

I partially disagree. A shim that wraps the old methods behind a `step_track` facade without changing behavior adds code without removing risk -- it is the *behavioral* change that causes regressions, not the interface change. However, I agree the jump from Phase 1 to Phase 2 is the riskiest moment. The better mitigation is what my Round 1 item 3.2 proposed: make Phase 1 a passive observer (FSM mirrors state but does not drive behavior), then Phase 2 atomically switches authority. This avoids dual-truth *and* avoids a useless shim.

**Resolution:** Phase 1 FSM runs in shadow mode (asserts against actual behavior, never drives it). Phase 2 flips authority. No shim.

### 1.3 Disagreement with Gemini #6: `step_track` Internal Looping

Gemini says `step_track` should internally loop through transient transitions until reaching a stable state, to prevent the worker from having to decide whether to re-call.

I strongly disagree. This is exactly the cooperative scheduling violation I flagged in Round 1 (item 5.1). The worker runs multiple tracks in round-robin. If `step_track` internally loops through `SeekRequested -> WaitingForSource -> ApplyingSeek -> RecreatingDecoder -> AwaitingFirstChunk -> Decoding`, that is potentially unbounded work in a single step -- it starves other tracks. The contract must be: one FSM transition per `step_track` call, `StateChanged` counts as `Progress`, and the worker re-enters the round-robin. The worker already knows how to re-schedule a track that returns `Progress`.

**Resolution:** Single FSM transition per call. Worker treats `StateChanged` as `Progress` and gives other tracks a turn before returning.

### 1.4 Disagreement with Codex #4: Splitting RecreatingDecoder by Cause

Codex argues that `RecreatingDecoder` conflates seek recovery with format boundary recreation, and that format-boundary recreation happens "during steady decode with no seek epoch."

This is partially correct but the proposed fix (splitting recreate flows) adds more states without solving a real bug. Looking at the actual code (`source.rs:976-1011`), format-boundary recreation happens in `handle_decode_eof()` and `handle_decode_error()` -- these are already within the decode loop, not within the seek path. The FSM design already carries `RecreateCause` (which has `FormatBoundary`, `SeekRecovery`, `CodecChange` variants), and `AwaitingFirstChunk` carries an `epoch` that can be `0` or the current non-seek epoch for format-change recreation. The cause enum is the right discriminator, not separate states.

However, Codex correctly identifies that `AwaitingFirstChunk` as written implies a seek. The fix is to make the `epoch` and `skip` fields optional in `AwaitingFirstChunk`, or rename it to `AwaitingFirstOutput` with optional seek context.

**Resolution:** Keep single `RecreatingDecoder` with `RecreateCause`. Rename `AwaitingFirstChunk` to something like `AwaitingResume` and make seek context optional.

### 1.5 Disagreement with Codex #5: TrackStep Duplicates Internal State

Codex says `TrackStep::Blocked` vs `WaitingForSource` and `TrackStep::AtEof` vs `TrackState::AtEof` are redundant, and that `Idle` is ambiguous.

I partially agree about `Idle` being ambiguous (I said the same in Round 1, 5.2), but I disagree that `TrackStep` duplicates state. `TrackStep` is the *return value* of a single step -- it tells the scheduler what happened in this step. `TrackState` is the *internal state* of the FSM. They serve different audiences: the scheduler uses `TrackStep` to decide whether to yield or re-schedule; diagnostics uses `TrackState` for full context. The duplication is intentional and correct -- it is the same pattern as `StepResult::Progress/NoProgress` today, just richer.

**Resolution:** Keep `TrackStep` as scheduler-oriented return value. Remove `Idle`. Keep `TrackState` as internal FSM state. They are not the same thing and should not be merged.

---

## Part 2: Top 5 Most Critical Issues (Ranked by Impact)

### Rank 1: Missing `Decoding -> WaitingForSource` Transition

**All three analyses flagged this.** This is the single most important gap because it is the *exact failure mode* described in the design document's Section 2.3 (`test6.log` hang). The FSM was designed to fix this hang, but as written it cannot represent the very scenario it was created for: source enters `WaitingDemand` during normal playback (not during seek).

Looking at the real code, `is_ready()` (`source.rs:1152-1158`) returns `false` for `WaitingDemand`, causing `TrackSlot::step()` to return `NoProgress` at line 251. The worker parks with `IDLE_TIMEOUT` (10ms). There is no state to observe, no reason propagated, and if the hang detector fires, the log says only "no progress" without any actionable context. This is precisely the gap the FSM must fill.

**Severity:** This is the motivating bug. If the FSM omits this transition, the refactor fails its primary goal.

**Resolution:** Add `Decoding -> WaitingForSource { epoch: None, target: None, reason, attempt: 0 }`. Make epoch/target optional (or use a separate `BlockedOnSource` state as I proposed in Round 1).

### Rank 2: Silent Seek-Abandon Path (source.rs:1242-1255)

**All three analyses flagged this.** The current code at `source.rs:1242-1255` is deeply problematic: after `MAX_SEEK_RETRY` (3) failures, it silently stamps the new epoch and clears `seek_pending`, resuming playback from whatever position the stream happens to be at. The user requested seek to 2:30, the seek failed three times, and playback silently resumes at some arbitrary offset with no notification.

The design document explicitly forbids this (Section 8.5), but does not specify what the consumer should *do* when it receives `Failed(SeekApplyExhausted)`. If the track goes to `Failed` (terminal), the user loses playback entirely. If a `SeekRejected` event is emitted and the track reverts to `Decoding`, the consumer still needs to know the seek did not land where expected.

**Severity:** Silent data corruption (playing wrong audio position) is worse than a hang -- hangs are detectable, silent wrong-position playback is not.

**Resolution:** Emit `SeekRejected { epoch, target, attempts }` event. Transition to `Decoding` (not `Failed`) at whatever position the decoder happens to be. The consumer and diagnostics can observe the rejection. This is better than `Failed` because the track is still functional -- it just did not honor the seek.

### Rank 3: TrackStatus Synchronization / Lost Wakeup

**All three analyses flagged this.** The design says "worker wakes consumer on state transition" and "shared TrackStatus" but specifies no synchronization protocol. The classic race is:

1. Consumer checks TrackStatus: `Decoding` (stale)
2. Worker transitions to `WaitingForSource`, writes TrackStatus, calls wake
3. Consumer registers wait (after step 1 check but before parking)
4. Wake is lost because consumer had not yet parked
5. Consumer parks forever

Looking at the actual wake mechanism (`ThreadWake`), this is a condvar-based parker. The check-then-park pattern is already used in `recv_outcome_blocking()` (`audio.rs:466`). The fix is not architectural -- it is ensuring the existing check-park-recheck loop also consults `TrackStatus` before parking. But the design document must specify this explicitly.

**Severity:** Potential hang in the new system, which is ironic given the FSM was designed to eliminate hangs.

**Resolution:** TrackStatus must be updated under the same lock (or with the same atomic sequence counter) that the wake signal uses. Consumer must: (1) check TrackStatus, (2) if no data and not terminal, register wait, (3) re-check TrackStatus after registering, (4) only then park. Document this in Section 10.1.

### Rank 4: Superseding Seek Must Be a Global Preemption Rule

**Gemini and Codex flagged this. I flagged the `AwaitingFirstChunk` case specifically.** The state chart only shows `WaitingForSource -> SeekRequested` for superseding seeks. But a new seek can arrive in *any* non-terminal state. Looking at the worker code (`audio_worker.rs:223-229`), the seek detection is done in `TrackPhase::Decoding` by checking `timeline.is_flushing() || timeline.is_seek_pending()`. But in `PendingReset` (line 213-220), the worker calls `is_ready()` and then `apply_pending_seek()` -- it does not first check whether a *newer* seek has superseded the current one.

In practice, this works today because `apply_pending_seek()` re-reads the epoch from Timeline. But the FSM design must make this explicit: any non-terminal state can transition to `SeekRequested` when a newer epoch is observed. Otherwise, the FSM will be implemented with explicit match arms that forget one of the intermediate states.

**Severity:** Without this rule, rapid-seek sequences will deadlock or produce stale seeks.

**Resolution:** Add a universal transition rule: "For every non-terminal state, if `timeline.seek_epoch() > self.epoch`, transition to `SeekRequested` with the new epoch." This should be checked at the *top* of `step_track`, before any state-specific logic.

### Rank 5: Dual-Truth During Migration (Timeline vs TrackState)

**Codex and I flagged this.** During Phase 1, both `Timeline` and `TrackState` claim to know the seek lifecycle. During Phase 2, `step_track` replaces the worker's seek detection, but `Audio::seek()` still writes to `Timeline`. During Phase 3, `ConsumerPhase` replaces `pending_seek_epoch`, but `Timeline` still holds the epoch.

The risk is not theoretical. Consider: Phase 2 is deployed, `step_track` reads epoch from `TrackState`, but `Audio::seek()` (on the consumer thread) writes epoch to `Timeline`. If `TrackState` is not updated from the same source, the FSM and Timeline diverge. Every seek is a split-brain risk.

**Severity:** This is the migration's Achilles' heel. Getting it wrong produces exactly the kind of subtle seek-lifecycle bugs the refactor is meant to eliminate.

**Resolution:** Define an authority matrix per phase (Codex's suggestion is correct here):

| Concern | Phase 1 Authority | Phase 2 Authority | Phase 3 Authority |
|---|---|---|---|
| Seek epoch | Timeline | Timeline (read by step_track) | Timeline (but exposed via ConsumerPhase) |
| Seek completion | Timeline | TrackState (SeekRequested -> Decoding) | TrackState |
| Source readiness | TrackSlot.is_ready() | step_track Blocked return | step_track |
| Consumer seek state | pending_seek_epoch | pending_seek_epoch | ConsumerPhase |

Phase 1 FSM runs in shadow mode and asserts parity. Phase 2 flips producer authority. Phase 3 flips consumer authority. At no point do two authorities drive behavior simultaneously.

---

## Part 3: New Insights Not Covered in Round 1

### 3.1 `pending_fetch` Stale-Epoch Contamination

None of the three analyses caught this specific race. Looking at `audio_worker.rs:234-245`:

```
if let Some(fetch) = self.pending_fetch.take() {
    match self.data_tx.try_push(fetch) { ... }
}
```

When a seek arrives, the worker clears `pending_fetch` at line 207 (`self.pending_fetch = None`). But there is a window: if `step()` decodes a chunk, it goes to `pending_fetch` because the ringbuf is full (line 263-265), and then *before the next step*, a seek arrives. The next `step()` enters `PendingReset` (line 208) and clears `pending_fetch`. This is correct today.

However, in the new FSM, if `step_track` is single-transition and returns `StateChanged(SeekRequested)`, the *next* call to `step_track` might try to push the pending fetch before processing the seek. The FSM must enforce: entering `SeekRequested` unconditionally drops `pending_fetch`. This is a scheduler concern (owned by `TrackSlot`), not an FSM concern -- but the design document does not mention it.

**Resolution:** Add to Section 8.3: "Entering any seek-related state (`SeekRequested`) must clear `pending_fetch` in the scheduler slot."

### 3.2 `DecodeError::Interrupted` Semantics Change Under step_track

Currently, `StreamAudioSource::next()` (the `FallibleIterator` impl, `source.rs:1034-1097`) checks `timeline.is_flushing()` at the top of each decode loop iteration and returns `Err(DecodeError::Interrupted)`. The caller (`fetch_next`, line 1128) catches this and returns an empty non-EOF fetch, which the worker treats as no-op.

Under `step_track`, if the FSM is driving transitions, the decode loop should not independently check Timeline for seek signals -- that would bypass the FSM. But removing the check means Symphonia's internal read loop (which calls `wait_range`) cannot be interrupted when a seek arrives. The design document does not address how decode-loop interruption works when the FSM owns seek detection.

This is a real problem because Symphonia can block inside `read()` for hundreds of milliseconds waiting for bytes, and the only current escape hatch is the `is_flushing()` guard.

**Resolution:** The FSM should set a cancellation/interruption flag that the decode loop checks, rather than having the decode loop read Timeline directly. This flag is set by the FSM when transitioning to `SeekRequested`. Mechanically, this could be a simple `AtomicBool` that the `Read` impl checks. Add this to the design as a Phase 2 requirement.

### 3.3 Effects Chain Reset Timing Not Specified

The current code resets effects at several points: on format change (`source.rs:988`), on boundary recovery (`source.rs:1017`), and on seek (`source.rs:500`). The FSM design does not specify which transitions trigger effects reset. If effects are not reset on every `RecreatingDecoder -> ApplyingSeek` or `SeekRequested -> *` transition, stale reverb/EQ tails bleed across seek points.

**Resolution:** Add to Section 7: "Effects chain MUST be reset on entry to `SeekRequested` and on entry to `RecreatingDecoder`. Effects flush MUST be performed on entry to `AtEof`."

### 3.4 `base_offset` Correctness After Failed Seek-Recovery

Looking at `source.rs:467-489`, `recreate_decoder` unconditionally updates `self.cached_media_info` and `self.base_offset` *before* checking if the factory succeeds. If the factory returns `None`, the base_offset and cached_media_info are now wrong for the old decoder (which is still alive). The next decode attempt uses stale metadata. Under the FSM, `DecoderSession` bundles these together, but the design must specify: `DecoderSession` is only replaced atomically on successful factory return. Never update base_offset or cached_media_info speculatively.

**Resolution:** Add to Section 9 (`DecoderSession`): "DecoderSession must be constructed atomically. If the factory fails, the old DecoderSession remains unchanged. No partial updates to base_offset or cached_media_info are permitted."

### 3.5 Hang Detector Integration Not Addressed

The current worker loop has `hang_tick!()`, `hang_reset!()`, and `#[kithara_hang_detector::hang_watchdog]` annotations (`audio_worker.rs:305, 367-391`). The decode loop in `StreamAudioSource::next()` also has its own `#[hang_watchdog]` (`source.rs:1034`). The FSM design replaces both control paths but says nothing about where hang detection annotations go.

Under `step_track`, the natural place for `hang_tick!()` is after `step_track` returns `Blocked` or `NoProgress`. But the per-decode-chunk `hang_tick!()` inside the decode loop (`source.rs:1037`) would be lost if the decode loop is subsumed by the FSM. This matters because a single decode call can block for seconds in `wait_range`.

**Resolution:** Add to Section 10: "The hang detector must tick on every `step_track` call that returns non-terminal non-progress. The decode loop (inside `Decoding` state) must retain its own `hang_tick!()` for intra-step stall detection."

---

## Part 4: Synthesis and Recommended Priority

Combining all three analyses, the implementation order should be:

1. **Fix the state chart first** (add missing transitions: `Decoding -> WaitingForSource`, universal `-> SeekRequested` preemption, `RecreatingDecoder -> WaitingForSource`, `WaitingForSource -> AtEof`).

2. **Define the authority matrix** before writing any code. This is the migration's safety net.

3. **Implement Phase 0** with FSM types, transition tests, and shadow-mode assertion harness.

4. **Address `pending_fetch` clearing, decode-loop interruption, and effects reset** as explicit design constraints before Phase 2.

5. **Leave `Closed`/`Shutdown` state out.** Cancellation is orthogonal.

The three strongest contributions across all analyses were:
- Gemini's identification of the `Decoding -> WaitingForSource` gap (shared by all three, but Gemini framed it most crisply as "rebuffering")
- Codex's authority matrix / dual-truth concern (most specific about the migration danger)
- My Round 1 identification of `step_track` conflating FSM advance with output production (cooperative scheduling preservation)

The weakest proposals were:
- Gemini's `Closed` state (unnecessary given `CancellationToken`)
- Gemini's internal-loop `step_track` (violates cooperative scheduling)
- Codex's proposal to split `RecreatingDecoder` into separate states (cause enum is sufficient)
