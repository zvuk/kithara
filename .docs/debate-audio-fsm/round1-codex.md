# Round 1 — Codex Analysis

## 1. Missing Decode-Starvation Path (Decoding -> WaitingForSource)
The document says the observed hang is `WaitingDemand` during ordinary decode, but `TrackState` has no `Decoding -> WaitingForSource -> Decoding` path.
**Fix:** Add decode-starvation transition, or split `WaitingForSource` into `WaitingForDecode` and `WaitingForSeek`.

## 2. Superseding Seeks Not Global Preemption Rule
Only `WaitingForSource` explicitly transitions back to `SeekRequested`. Missing for `ApplyingSeek`, `RecreatingDecoder`, `AwaitingFirstChunk`.
**Fix:** Define "higher seek epoch wins" as transition from every non-terminal state, suppress stale events.

## 3. Incomplete SourcePhase -> TrackState Mapping
`WaitingReason` only covers transient waits. `Cancelled`, `Stopped`, `Eof`, `Seeking` from SourcePhase have no defined mapping.
**Fix:** Add normative mapping table from every `SourcePhase` to `TrackState`/`TrackFailure`.

## 4. RecreatingDecoder Conflates Seek Recovery with Format Boundary
Format-boundary recreation happens during steady decode with no seek epoch. Chart pushes recreate toward `AwaitingFirstChunk` which is seek-specific.
**Fix:** Split recreate flows by cause, or carry optional seek context.

## 5. TrackStep Duplicates Internal State
`TrackStep::Blocked` vs `WaitingForSource`, `TrackStep::AtEof` vs `TrackState::AtEof`. `Idle` is ambiguous.
**Fix:** Make `step_track` scheduler-oriented and single-step with structured result.

## 6. TrackStatus Concurrency Not Safe
Without versioned publish protocol, consumer can observe empty queue + stale status, park, and miss state-only wake.
**Fix:** Add monotonic status sequence or control-event queue. Double-check after registering wake.

## 7. Migration Creates Long Dual-Truth Periods
Phase 1/2 introduce `TrackState` but `pending_decode_started_epoch`, `pending_seek_epoch`, and `eof` still drive behavior.
**Fix:** Add authority matrix per phase. Switch each behavior atomically.

## 8. Retry Semantics Underdefined
`attempt: u8` fields without carry/reset rules. Current code has distinct retry behaviors for outer seek and post-seek decode recovery.
**Fix:** Model retry budget in `SeekContext`/`RecoverContext`, document carry/reset rules.

## 9. Missing Race Scenario Tests
No tests for: superseding seek during `ApplyingSeek`/`RecreatingDecoder`/`AwaitingFirstChunk`, state-only wakeups with empty PCM queue, old-epoch `pending_fetch` surviving backpressure, `Cancelled`/`Stopped` source phases.
**Fix:** Add mandatory tests before implementation plus event-order tests.
