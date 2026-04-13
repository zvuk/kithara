# Phase 02 Attempt 5 — RC2 Handoff

## Branch: `phase02-attempt5`

## Current state

All changes are uncommitted. RC1 (tokio::select! cancellation-safety in Downloader) is fixed and verified — `config_with_downloader` passes.

**Baseline (last full run before any RC2 work):** 1833 tests, 19 FAIL, 1 FLAKY.

The 19 failures are all in `kithara-hls` integration tests. They are seek-related or ABR-related and share a common trait: after the queue-based `poll_next` rewrite, these tests stopped working. Before the rewrite (on `main`), they all pass.

## What was rewritten

The HLS download logic was rewritten from a monolithic `poll_next` with inline cursor/planning to a queue-based model:

- `crates/kithara-hls/src/scheduler/queue.rs` (NEW) — `DownloadQueue` with `do_rebuild()` + `pop()` + `in_flight` tracking
- `crates/kithara-hls/src/inner.rs` — `HlsPeer::poll_next` now calls `queue.rebuild_if_needed()` + `queue.pop()` + `build_fetch_cmd()`
- `crates/kithara-hls/src/source/source_impl.rs` — `DemandSlot` is now legacy (not consumed by new poll_next)

## What you must do

1. Run `just test`, save full output to `.docs/test-run-rc2-baseline.log`
2. Analyse every failing test's panic message and logs from that file
3. Group failures by actual root cause (not by test name)
4. Find the root cause that affects the most tests
5. Write a minimal, targeted fix for that one root cause
6. Verify: the sentinel tests for that cause pass, no new regressions (`just test`)
7. Repeat until all green

## Rules

- Do NOT guess. Instrument, reproduce, measure — then propose a fix.
- One root cause at a time. One fix at a time.
- After each fix, run `just test` to verify zero regressions.
- If a fix increases the total failure count, revert immediately.
- Read AGENTS.md and follow repo conventions.

## Key files to read

- `crates/kithara-hls/src/scheduler/queue.rs` — the new download queue
- `crates/kithara-hls/src/inner.rs` — poll_next + build_fetch_cmd
- `crates/kithara-hls/src/source/source_impl.rs` — wait_range, commit_seek_landing, demand_range
- `crates/kithara-hls/src/source/seek.rs` — apply_seek_plan
- `crates/kithara-hls/src/inner.rs:350` — make_invalidation_callback (eviction → StreamIndex removal)
- `crates/kithara-stream/src/stream.rs` — Stream::read, Stream::seek
- `.docs/phase02-test-failures-analysis.md` — earlier analysis (RC1 fixed, RC2/RC3 described but hypotheses may be wrong)

## What was tried and failed

Several fixes were attempted for RC2 and reverted because they increased the failure count:
- Adding `max_lookahead` parameter to DownloadQueue
- Removing backfill from do_rebuild
- Setting byte_position early in apply_seek_plan
- Changing unwrap_or(0) to unwrap_or_else(last_segment)

All reverted. The codebase is clean — only RC1 fix present.
