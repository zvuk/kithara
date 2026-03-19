# HLS Layout Source-of-Truth Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox syntax for tracking.

**Goal:** Make `StreamIndex` the only source of truth for HLS layout, segment visibility, invalidation recovery, and stream byte length, while making the ABR atomic the only source of truth for the current target variant. Fix the DRM auto-switch replay bug without symptom patches.

**Architecture:** Move all HLS decisions about visibility, loading, EOF, and stream length onto `StreamIndex + PlaylistState`. `Timeline` stays a concrete runtime playback-state object only; it must not store or derive HLS layout/length facts. `ABR` and its atomic are the only authority for the current target variant; no HLS cache may compete with that fact. `StreamIndex` remains the only authority for segment ownership in the logical layout; no downloader/source cache may compete with that fact either. Keep `on_invalidated -> StreamIndex.remove_resource -> notify_all` as the authoritative invalidation path, and fix the missing recovery step after invalidation: a layout hole must trigger the existing HLS demand path, which already reopens from `AssetStore` when possible and only falls back to network on a real store miss. Remove HLS writes to `Timeline.total_bytes`, make downloader suppression depend only on current `StreamIndex` visibility, and preserve the mixed auto-switch layout across replay after `seek(0)`.

**Tech Stack:** Rust, `StreamIndex`, `PlaylistState`, `AssetStore` invalidation callback, `cargo test`

**Related docs:** `.docs/refactor-stream-index.md`, `.docs/2026-03-13-hls-seek-root-cause-refactor-plan.md`

---

## Current Status (2026-03-15)

- [x] architectural decision: do not turn `Timeline` into a trait for this fix
- [x] architectural decision: `Timeline` remains runtime playback/seek state only
- [x] architectural decision: HLS byte length must come only from `StreamIndex.effective_total(...)`
- [x] architectural decision: the only source of truth for the current target variant is the ABR atomic
- [x] architectural decision: `StreamIndex` is the only source of truth for logical segment ownership in the composed stream
- [x] `StreamIndex.remove_resource(...)` invalidates current layout visibility and keeps following byte offsets stable instead of compacting the stream.
- [x] `source` no longer turns layout holes below effective total into `EOF` / `Data(0)`.
- [x] on-demand recovery now resolves `(variant, segment)` from the current logical layout, not from the current ABR variant.
- [x] no backend-specific invalidation gates remain in HLS.
- [x] HLS production code no longer reads or writes `Timeline.total_bytes`; HLS derives byte length only from `StreamIndex.effective_total(...)`
- [x] `kithara-file` production code no longer reads or writes `Timeline.total_bytes`; file byte length is now shared through `FileCoord`
- [x] `Timeline::total_bytes()` / `Timeline::set_total_bytes(...)` were removed after HLS and `kithara-file` stopped using them in production code
- [x] stale current-variant cache `last_committed_variant` is gone from HLS
- [x] `variant_fence` in `source` is now treated only as a read/seek fence; `resolve_current_variant()` uses the ABR atomic
- [x] `CachedAssets` no longer evicts active resource handles under LRU pressure; active resources stay discoverable until they stop being active
- [x] focused assets regressions exist for:
  - `disk_resource_state_keeps_active_status_after_handle_cache_eviction`
  - `active_resource_survives_lru_eviction_pressure`
  - `reopened_committed_processed_resource_without_ctx_reads_committed_bytes`
- [x] focused regression tests exist for:
  - `read_at_missing_segment_before_effective_total_returns_retry`
  - `segment_loaded_for_demand_requires_visible_layout_entry`
  - `remove_resource_invalidates_visible_segment`
  - `queue_segment_request_uses_layout_variant_for_invalidated_prefix`
  - `commit_seek_landing_uses_layout_variant_for_invalidated_prefix`
- [x] focused regression test exists for stale demand replacement:
  - `wait_range_replaces_mismatched_pending_request_for_same_epoch`
- [x] targeted diagnostic confirmed that the old `drm_eph_auto` replay failure at `152_859` bytes was partly a test-scanner artifact: phase 2 collapsed `VariantChange` into apparent `EOF`
- [x] heavy regression status was re-measured after the assets/HLS fixes: `drm_stream_byte_integrity` passed `12/12`
- [x] `drm_stream_byte_integrity_drm_disk_auto` passed repeated single-test re-runs after fixing stale pending demand replacement in `wait_range`
- [x] `drm_stream_byte_integrity` now fails Phase 1 on incomplete reads: the test records read errors/deadline exits and asserts `total_read ~= stream_len`, so early-prefix false-green passes are no longer possible
- [ ] the replay-scanner change in `tests/tests/kithara_hls/drm_stream_integrity.rs` is test-only and must remain isolated from HLS production fixes
- [x] `Timeline.total_bytes` was removed from `kithara-stream`; stale-EOF tests now use source-specific length instead of a timeline cache

## Plan Discipline

- Architectural changes that are on-plan:
  - move HLS byte length onto `StreamIndex.effective_total(...)`
  - keep ABR atomic authoritative for current target variant
  - keep `variant_fence` as a fence only
  - keep `on_invalidated -> StreamIndex.remove_resource -> notify_all` authoritative
- Diagnostic or test-only deviation already taken:
  - phase-2 replay scanner now clears `variant_fence` on `VariantChange`, matching phase 1
  - Phase 1 in `drm_stream_byte_integrity` now asserts read completeness (`total_read ~= stream_len`) and treats read errors/deadline exits as test failures
  - this change proved that one previously reported auto-DRM replay failure was a scanner artifact, not by itself proof that the remaining disk failures are fixed
- Rule for the next steps:
  - do not patch HLS production code around the integration test symptom
  - before any new product fix, reproduce the remaining instability with a short repeat pass; if it does not reproduce, close this bug class and move on to the next planned cleanup

---

## Chunk 1: Lock the failure down with focused tests

### Task 1: Add unit tests that describe the real bug shape

**Files:**
- Modify: `crates/kithara-hls/src/source.rs`
- Modify: `crates/kithara-hls/src/downloader.rs`
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Add a failing `read_at` hole test in `source.rs`**

Add a unit test named `read_at_missing_segment_before_effective_total_returns_retry`.

Scenario:
- `PlaylistState` reports a known total via size map
- `StreamIndex` has a committed prefix but no segment covering the requested offset
- requested `offset` is still below `segments.effective_total(...)`
- `read_at` must return `ReadOutcome::Retry`, not `ReadOutcome::Data(0)`

- [ ] **Step 2: Add a failing suppression test in `downloader.rs`**

Add a unit test named `segment_loaded_for_demand_requires_visible_layout_entry`.

Scenario:
- segment metadata exists in per-variant storage
- current layout no longer maps the requested offset to that segment
- resources may still exist
- `segment_loaded_for_demand` must return `false`

- [ ] **Step 3: Add a failing invalidation test in `stream_index.rs`**

Add a unit test named `remove_resource_invalidates_visible_segment`.

Scenario:
- commit a segment with `init_url` and `media_url`
- invalidate one resource key
- the segment must stop being readable from current layout until recommitted

- [ ] **Step 4: Run just the new tests and verify they fail**

Run:
- `cargo test -p kithara-hls read_at_missing_segment_before_effective_total_returns_retry --lib`
- `cargo test -p kithara-hls segment_loaded_for_demand_requires_visible_layout_entry --lib`
- `cargo test -p kithara-hls remove_resource_invalidates_visible_segment --lib`

Expected:
- each test fails for the current wrong reason

- [ ] **Step 5: Commit the red tests**

`git add crates/kithara-hls/src/source.rs crates/kithara-hls/src/downloader.rs crates/kithara-hls/src/stream_index.rs`

`git commit -m "test(hls): lock layout hole and invalidation regressions"`

---

## Chunk 2: Expand StreamIndex into the real state model

### Task 2: Add StreamIndex APIs for visibility, range lookup, and invalidation

**Files:**
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Add minimal API needed by source and downloader**

Add methods with these semantics:
- `range_for(variant, segment_index) -> Option<Range<u64>>`
- `is_visible(variant, segment_index) -> bool`
- `visible_segment_at(offset) -> Option<SegmentRef<'_>>`
- `remove_resource(key: &ResourceKey) -> bool`

Implementation rules:
- `visible_segment_at` can delegate to current `find_at_offset`
- `is_visible` must be defined by `byte_map`, not by `variant_segments`
- `remove_resource` must remove the affected committed entry or mark it unreadable in a way that removes its byte coverage from current layout
- keep the API minimal; do not add generic helpers that are not used immediately

- [ ] **Step 2: Add the internal reverse mapping needed for invalidation**

Add only the smallest structure required to answer:
- which `(variant, segment_index)` owns this `ResourceKey`
- whether the key belongs to init or media

Do not add a broad “resource registry” abstraction. Keep it local to `StreamIndex`.

- [ ] **Step 3: Make `effective_total(&dyn PlaylistAccess)` the canonical HLS length API**

Do not add a second length method. Reuse the existing `effective_total`.

- [ ] **Step 4: Run the `stream_index` test subset**

Run:
- `cargo test -p kithara-hls stream_index --lib`

- [ ] **Step 5: Commit**

`git add crates/kithara-hls/src/stream_index.rs`

`git commit -m "feat(hls): extend stream index for visibility and invalidation"`

---

## Chunk 3: Rewrite downloader suppression around StreamIndex

### Task 3: Remove layout-blind suppression and cached total refresh

**Files:**
- Modify: `crates/kithara-hls/src/downloader.rs`

- [ ] **Step 1: Delete HLS cached-total helpers**

Delete these functions completely:
- `reconcile_total_bytes`
- `refresh_variant_total_bytes`
- `should_prepare_variant_totals`

Delete their call sites in:
- `commit_segment`
- `ensure_variant_ready`
- `reset_for_seek_epoch`

- [ ] **Step 2: Rewrite `segment_loaded_for_demand`**

New decision order:
1. ask `StreamIndex` whether the segment is visible in current layout
2. if not visible, return `false`
3. if visible, check resource presence for the exact committed entry
4. only then allow suppression

- [ ] **Step 3: Fix re-demand after invalidation**

Rules:
- if `on_invalidated` removed a segment from `StreamIndex`, the next demand for that `(variant, segment_index)` must not be suppressed
- pending-request dedupe must not strand the segment in a permanent hole state
- replay after `seek(0)` must be allowed to requeue the missing prefix segments

Do not reintroduce `is_duplicate_commit` or any equivalent write gate.

- [ ] **Step 4: Rewrite `handle_tail_state` and `reset_for_seek_epoch`**

Rules:
- `handle_tail_state` must compute stream end from `segments.effective_total(self.playlist_state.as_ref())`
- `reset_for_seek_epoch` must stop preserving or refreshing cached `total_bytes`
- keep `Timeline` updates limited to seek state, byte cursor, download cursor, and EOF flag
- do not introduce any new HLS-derived field into `Timeline`

- [ ] **Step 5: Run focused downloader tests**

Run:
- `cargo test -p kithara-hls segment_loaded_for_demand --lib`
- `cargo test -p kithara-hls reset_for_seek_epoch --lib`
- `cargo test -p kithara-hls handle_tail_state --lib`

- [ ] **Step 6: Commit**

`git add crates/kithara-hls/src/downloader.rs`

`git commit -m "refactor(hls): make downloader suppression layout-aware"`

---

## Chunk 4: Keep invalidation authoritative and recover through AssetStore

### Task 4: Turn invalidation holes into read-through recovery

**Files:**
- Modify: `crates/kithara-hls/src/inner.rs`
- Modify: `crates/kithara-hls/src/source.rs`
- Modify: `crates/kithara-hls/src/downloader.rs`
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Preserve the existing invalidation contract**

Keep this exact behavior:
- `on_invalidated` receives the invalidated `ResourceKey`
- HLS calls `StreamIndex::remove_resource`
- HLS notifies waiters via `coord.condvar`

Do not add backend-specific gates. Do not weaken invalidation into a hint.

- [ ] **Step 2: Make invalidation observable by blocked readers and replay**

If an entry disappears:
- `wait_range` must wake and re-evaluate
- `read_at` and replay after `seek(0)` must see a hole and requeue demand
- no stale ready state may survive in current layout

- [ ] **Step 3: Recover the segment through the existing demand path**

Rules:
- after invalidation, downloader/source must requeue the missing `(variant, segment)` through the normal HLS demand path
- do not add special `AssetStore` recovery code in HLS; the existing store/open path already re-caches when the resource still exists
- if the resource is still available, the normal path must recommit the segment into `StreamIndex` without going to network
- only if `AssetStore` misses should downloader fetch from network and then recommit

This recovery logic must be identical for disk and ephemeral; the distinction comes from whether `AssetStore` can reopen the resource, not from explicit HLS mode gates.

- [ ] **Step 4: Add or update unit tests around the invalidation recovery path**

Add tests covering:
- `asset_invalidation_removes_layout_visibility`
- `read_at_after_invalidation_returns_retry_before_effective_total`
- `recovery_recommits_segment_from_store_before_network_fetch`

- [ ] **Step 5: Run targeted invalidation tests**

Run:
- `cargo test -p kithara-hls invalidation --lib`
- `cargo test -p kithara-hls read_at_missing_segment_before_effective_total_returns_retry --lib`

- [ ] **Step 6: Commit**

`git add crates/kithara-hls/src/inner.rs crates/kithara-hls/src/source.rs crates/kithara-hls/src/downloader.rs crates/kithara-hls/src/stream_index.rs`

`git commit -m "fix(hls): recover invalidated segments through asset store"`

---

## Chunk 5: Preserve mixed auto layout across seek and replay

### Task 5: Keep auto-switch layout authoritative after `seek(0)`

**Files:**
- Modify: `crates/kithara-hls/src/source.rs`
- Modify: `crates/kithara-hls/src/downloader.rs`
- Modify: `crates/kithara-hls/src/stream_index.rs`

- [ ] **Step 1: Add a focused regression test for replay after an auto switch**

Add a unit test in `source.rs` that builds this exact shape:
- commit variant-0 prefix segments into `StreamIndex`
- switch the tail to variant 1 with `segments.switch_variant(...)`
- commit at least one tail segment for variant 1
- invalidate a prefix resource so replay must recover the prefix through the hole path
- simulate the `seek(0)` / replay path

Expected:
- recovery request for the invalidated prefix targets variant 0
- the tail remains owned by variant 1 after replay planning
- replay does not collapse the whole layout back to variant 0

- [ ] **Step 2: Audit all seek/reset paths that can overwrite the mixed layout**

Inspect and narrow behavior in:
- `source.rs::classify_seek`
- `source.rs::apply_seek_plan`
- `source.rs::commit_seek_landing`
- `downloader.rs::reset_for_seek_epoch`

Rules:
- `seek(0)` replay must not replace an existing mixed auto layout with a single-variant baseline unless this is a true cross-variant reset
- if the layout is preserved, landing on segment 0 must not call `switch_variant(...)` in a way that truncates the switched tail
- downloader seek reset must not repopulate a new single-variant total/layout baseline that disagrees with `StreamIndex`
- no seek/reset path may consult `Timeline.total_bytes`; HLS byte length must be derived dynamically from `StreamIndex.effective_total(...)`

- [ ] **Step 2.1: Remove stale current-variant caches from switch classification**

Inspect and narrow behavior in:
- `downloader.rs::classify_variant_transition`
- `downloader.rs::commit_segment`
- `downloader.rs::update_variant_tracking`
- `source.rs::resolve_current_variant`

Rules:
- the only source of truth for the current target variant is `ABR` and its atomic
- `last_committed_variant` must not decide whether a recovered prefix segment is a variant switch
- `variant_fence` may constrain replay/read planning, but it must not redefine the current target variant
- any decision that mutates `StreamIndex` layout must use `StreamIndex` ownership for that `segment_index`, not a cached “current variant”

- [ ] **Step 3: Fix the layout-preservation bug with the minimal state transition change**

Implementation target:
- keep the mixed `variant_map` authoritative across replay when the seek is logically same-layout
- avoid any `switch_variant(...)` / `reset_to(...)` call that rewrites the full tail during replay landing
- avoid any `switch_variant(...)` triggered only because `last_committed_variant` disagrees with a recovered prefix segment
- preserve demand/recovery behavior for the invalidated prefix segment

- [ ] **Step 4: Run the focused replay tests**

Run:
- `cargo test -p kithara-hls queue_segment_request_uses_layout_variant_for_invalidated_prefix --lib`
- `cargo test -p kithara-hls seek --lib`
- the new replay-layout regression test

Expected:
- layout-aware prefix recovery still passes
- replay preserves the mixed layout instead of collapsing to the landed variant

- [ ] **Step 5: Commit**

`git add crates/kithara-hls/src/source.rs crates/kithara-hls/src/downloader.rs crates/kithara-hls/src/stream_index.rs`

`git commit -m "fix(hls): preserve switched layout across replay"`

---

## Chunk 6: Remove HLS dependence on Timeline byte-length cache

### Task 6: Make HLS derive byte length dynamically from StreamIndex

**Files:**
- Modify: `crates/kithara-hls/src/source.rs`
- Modify: `crates/kithara-hls/src/downloader.rs`

- [ ] **Step 1: Add a single helper for dynamic total**

Add one internal helper only:
- `fn effective_total_bytes(&self) -> Option<u64>`

Rules:
- return `None` only when both committed watermark and metadata estimate are unavailable
- compute from `self.segments.lock_sync().effective_total(self.playlist_state.as_ref())`
- this helper is an HLS concern; do not move byte-length derivation into `Timeline`

- [ ] **Step 2: Replace all HLS reads and writes of `timeline.total_bytes()`**

Replace in:
- `fallback_segment_index_for_offset`
- `is_past_eof`
- `wait_range`
- `len`
- `reset_for_seek_epoch`
- `reconcile_total_bytes`
- `should_prepare_variant_totals`
- `refresh_variant_total_bytes`
- `handle_tail_state`

Rules:
- do not use `Timeline.total_bytes()` anywhere in HLS production logic after this task
- do not call `Timeline.set_total_bytes(...)` anywhere in HLS production logic after this task
- canonical HLS `byte_len` is `self.segments.lock_sync().effective_total(self.playlist_state.as_ref())`
- `Timeline` remains concrete and continues to own only seek/playback atomics

- [ ] **Step 3: Rewrite `read_at` hole handling**

If `visible_segment_at(offset)` returns `None`:
- if `offset` is at or past dynamic effective total, return `ReadOutcome::Data(0)`
- otherwise push or preserve demand and return `ReadOutcome::Retry`

- [ ] **Step 4: Update unit tests that currently set `timeline.set_total_bytes(...)`**

Rewrite them to:
- seed `PlaylistState` size map
- or seed `StreamIndex` committed layout

Specific stale tests to rewrite:
- exact EOF tests in `source.rs`
- short-read near EOF tests in `source.rs`

- [ ] **Step 5: Run the focused source test subset**

Run:
- `cargo test -p kithara-hls read_at --lib`
- `cargo test -p kithara-hls wait_range --lib`
- `cargo test -p kithara-hls phase_eof --lib`
- `cargo test -p kithara-hls reset_for_seek_epoch --lib`

- [ ] **Step 6: Commit**

`git add crates/kithara-hls/src/source.rs crates/kithara-hls/src/downloader.rs`

`git commit -m "refactor(hls): remove timeline byte-length cache from hls"`

---

## Chunk 7: Final cleanup and regression verification

### Task 7: Remove leftover HLS dependence on Timeline total_bytes and verify DRM auto

**Files:**
- Modify: `crates/kithara-hls/src/downloader.rs`
- Modify: `crates/kithara-hls/src/source.rs`
- Modify tests as needed

- [ ] **Step 1: Search for remaining HLS references**

Run:
- `rg -n "set_total_bytes\\(|total_bytes\\(\\)" crates/kithara-hls/src`

Target end state:
- no HLS production code reads `Timeline.total_bytes()`
- no HLS production code writes `Timeline.set_total_bytes(...)`

- [ ] **Step 2: Keep `Timeline.total_bytes` untouched outside HLS**

Do not refactor `kithara-stream` in this chunk.
This plan only removes HLS dependence on the field.

- [ ] **Step 2.1: Record follow-up architectural cleanup**

After HLS is green, decide separately whether to:
- remove `Timeline.total_bytes` from `kithara-stream` entirely
- or keep it only for non-HLS stream types

Do not block the HLS fix on that cross-crate cleanup.

- [ ] **Step 3: Run library tests**

Run:
- `cargo test -p kithara-hls --lib`

- [ ] **Step 4: Run the regression test that matters**

Run:
- `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`

Expected:
- all `drm_stream_byte_integrity` cases turn green
- specifically, disk-backed variants must recover after invalidation without reintroducing backend gates
- replay after `seek(0)` must restore full box coverage instead of stopping at a prefix or producing unknown box boundaries

- [ ] **Step 5: Save the result in the debug log**

Append outcome and any remaining failures to:
- `.docs/debug-log-2026-03-15.md`

- [ ] **Step 6: Commit**

`git add crates/kithara-hls/src/downloader.rs crates/kithara-hls/src/source.rs crates/kithara-hls/src/stream_index.rs crates/kithara-hls/src/inner.rs .docs/debug-log-2026-03-15.md`

`git commit -m "fix(hls): unify layout, invalidation, and dynamic stream length"`
