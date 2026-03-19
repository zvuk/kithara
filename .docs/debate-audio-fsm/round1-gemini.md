# Round 1 — Gemini Analysis

## 1. Missing "Rebuffering" Transition (Decoding -> WaitingForSource)
The state chart shows `ApplyingSeek -> WaitingForSource`, but lacks transition for source starvation during active playback. If `HlsSource` enters `WaitingDemand` while in `Decoding`, the FSM cannot reflect this.
**Fix:** Add `Decoding -> WaitingForSource`.

## 2. Superseding Seeks during Skip/Recreate
Transitions from `AwaitingFirstChunk`, `RecreatingDecoder`, and `ApplyingSeek` back to `SeekRequested` are missing. Rapid-seeks could get stuck.
**Fix:** Allow `* -> SeekRequested` for all non-terminal states.

## 3. "Lost Wakeup" in TrackStatus
Shared `TrackStatus` with "worker wakes consumer on state transition" has a classic lost wakeup race.
**Fix:** Check -> Park -> Re-check pattern. Status must be updated under lock or with thread-safe primitive.

## 4. Phase 1 "Flag Day" Risk
Replacing `fetch_next/is_ready` with `step_track` in Phase 2 breaks all `AudioWorkerSource` implementations at once.
**Fix:** Introduce shim layer in Phase 0.5 — implement `step_track` wrapping existing methods first.

## 5. Missing Decoder Failure Tests
No scenarios covering `RecreatingDecoder -> Failed` (OOM, corrupted header) or stall/resume tests.
**Fix:** Add tests for decoder recreation failure and network stall recovery.

## 6. `TrackStep::StateChanged` Ambiguity
If `step_track` returns `StateChanged`, should worker call again immediately? Risk of infinite loop or stalling.
**Fix:** `step_track` should internally loop through transient transitions until reaching a stable state.

## 7. Missing `Closed`/`Shutdown` State
User can stop/close a track before EOF. No FSM state represents this.
**Fix:** Add `Closed` terminal state for explicit resource release.
