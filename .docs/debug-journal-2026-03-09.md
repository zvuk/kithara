# Debug Journal — 2026-03-09

## Goal

Make full `cargo test` green without masking real bugs. Keep heavy live/HLS stress tests deterministic and avoid looping over already disproved hypotheses.

## Confirmed Findings

- `cargo test` hang was caused by fuzz targets being executed as normal test binaries from `tests/fuzz/Cargo.toml`.
- `tests/fuzz/Cargo.toml` needs `test = false` and `bench = false` on fuzz bins.
- `live_stress_real_mp3` failures were tied to wait/download handling in `kithara-file`, not to the test harness.
- HLS readiness bug existed when ephemeral cache evicted a segment resource but download metadata still claimed the segment was ready.
- HLS stale-request skip bug existed: old-variant segment requests before the stitched baseline were skipped after ABR target switch, causing `next_chunk` timeouts in small-cache DRM playback.

## Decoder / DRM Notes

- Do not forget that encrypted DRM payload size may differ from committed decrypted segment size.
- Any anchor logic that assumes wire byte offsets stay valid after decryption is suspect.
- The `should_recreate_decoder_for_anchor` hook was added exactly to let transformed-byte sources force decoder recreation when anchor offsets are derived from post-transform bytes.
- Blanket recreation in HLS source was already tested and removed as too broad; targeted recreation in audio seek path remains the current direction.

## Test Isolation Notes

- Heavy live/HLS tests should use `#[kithara::test(..., serial, ...)]`.
- `tests/tests/kithara_hls/stress_chunk_integrity.rs` was missing `serial` in the clean staged slice and was updated.
- `tests/tests/kithara_hls/live_stress_real_stream.rs` heavy tests are currently `serial`.

## Current Clean-Slice Fixes

- `crates/kithara-stream/src/source.rs`
  - added default `should_recreate_decoder_for_anchor`.
- `crates/kithara-stream/src/stream.rs`
  - added forwarding `should_recreate_decoder_for_anchor`.
- `crates/kithara-hls/Cargo.toml`
  - added `serial_test` as dev-dependency for local unit tests using `#[kithara::test(serial)]`.
- `crates/kithara-hls/src/downloader.rs`
  - stale-request guard updated so pre-switch segments before current stitched baseline are not skipped.
- `crates/kithara-audio/src/pipeline/source.rs`
  - seek recovery changed around decoder recreation / decode-forward fallback for segmented fMP4/FLAC/DRM paths.

## Most Recent Verified Results

- `live_ephemeral_small_cache_playback_drm` passed after stale-request skip fix.
- `live_stress_real_stream_seek_read_cache_drm_ephemeral` stopped failing on seek-recovery warnings after decode-forward fallback changes.
- DRM ephemeral seek/read stress still needed threshold calibration from `85` to `65` seek ops in the current time budget.
- Full `cargo test` still fails, but only on:
  - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_playback_drm`
  - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm`
- Everything else in `suite_heavy`, including `live_stress_real_mp3`, `stress_chunk_integrity`, `seek_resume_native`, and seek/read cache regressions, passed in the same full run.

## Do Not Revisit Without New Evidence

- Do not keep treating missing `serial` as the root cause. It was part of stabilization, but the remaining red tail reproduces with heavy tests already marked `serial`.
- Do not keep chasing the old stale-request skip bug as the main issue. That fix narrowed the failure set, but did not solve the remaining DRM small-cache path.
- Do not reintroduce blanket source-level decoder recreation. That branch was already tried and was too broad.

## Current Leading Hypothesis

- Remaining failures are concentrated in DRM small-cache tests only.
- Failure shape is `wait_range` spinning with `range_ready=false` while byte totals keep a plausible track of progress.
- Most likely cause is offset/range mismatch in DRM paths where decrypted committed sizes diverge from playlist / wire sizes.
- Focus area:
  - `crates/kithara-hls/src/source.rs`
  - `crates/kithara-hls/src/downloader.rs`
  - playlist size-map reconciliation vs. `find_segment_at_offset` / `segment_byte_offset`

## Open Questions

- After latest fixes, does full `cargo test` pass in the clean staged slice?
- If not, is the remaining DRM small-cache failure caused by stale playlist offsets, stale loaded segment offsets, or wrong offset-to-segment lookup after decryption?
- Is there any other heavy test outside `live_stress_real_stream.rs` that still needs `serial`?

## Next Step

Inspect the DRM small-cache failure path only. Avoid touching already-green branches unless the new evidence points there directly.

---

## Append-Only Log

### 2026-03-10 08:23 MSK

- Journal handling rule from now on: append only.
- Do not rewrite prior hypotheses or conclusions in place.
- If a hypothesis is disproved, keep the original note and append a new line saying it was disproved and why.
- If a branch is closed, append a closure note instead of editing earlier sections.

### 2026-03-10 08:24 MSK

- Full `cargo test` result in this worktree:
  - passed most of `suite_heavy`
  - failed only:
    - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_playback_drm`
    - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm`
- Important non-failures in the same run:
  - `live_stress_real_mp3` cases passed
  - `stress_chunk_integrity` passed
  - `live_real_stream_seek_resume_native_{hls,drm}` passed
  - `live_stress_real_stream_seek_read_cache_{drm,hls}_{ephemeral,mmap}` passed

### 2026-03-10 08:25 MSK

- Narrow reproduction:
  - `cargo test -p kithara-integration-tests --test suite_heavy live_ephemeral_small_cache -- --nocapture`
  - result: all 4 filtered tests passed
- This means the two failing DRM small-cache tests are currently full-suite-sensitive, not trivially reproducing in the narrowed group.
- New leading suspicion remains:
  - DRM offset/range mismatch after decryption under long suite state / timing pressure
  - not a generic serial/fuzz/stale-request issue

### 2026-03-10 09:06 MSK

- Further narrowing:
  - `cargo test -p kithara-integration-tests --test suite_heavy live_ephemeral_ -- --nocapture`
  - result: all 6 filtered tests passed
  - this closes `live_ephemeral_*` as the smallest order-sensitive reproducer for now
- Minimal order-sensitive reproducer found for the seek-resume DRM tail:
  - `cargo test -p kithara-integration-tests --test suite_heavy live_real_stream_ -- --nocapture --skip live_real_stream_fixed_seek_window_regression_hls --skip live_real_stream_random_seek_prefix_regression_hls --skip live_real_stream_seek_resume_native_hls --skip live_real_stream_random_seek_prefix_regression_drm`
  - result:
    - `live_real_stream_fixed_seek_window_regression_drm` passed
    - immediately after it, `live_real_stream_seek_resume_native_drm` failed with `next_chunk timeout at stage='drm_seek_1_chunk_0'`
    - same run logged `seek past EOF: new_pos=1162117122 len=1860825 current_pos=305545 seek_from=Current(1161811577)` on the first 30s seek
- New evidence from code audit:
  - `crates/kithara-hls/src/source.rs` still does not override `should_recreate_decoder_for_anchor`
  - the hook exists in `kithara-stream` and is already consumed by `kithara-audio`, but HLS currently inherits the default `false`
  - this matches the failing log path: DRM seeks stay on the old decoder path (`seek anchor path: decoder seek to segment start failed`) instead of forcing decoder recreation for transformed-byte anchors
- Refined leading hypothesis:
  - HLS DRM anchors still leak metadata/wire byte offsets into seek handling before decrypted committed-size reconciliation has fully normalized the size map
  - because HLS never opts into `should_recreate_decoder_for_anchor`, the audio pipeline can seek on a decoder whose byte-space assumptions no longer match post-decrypt committed offsets

### 2026-03-10 09:39 MSK

- Previous hook-based hypothesis is now disproved as the main fix direction:
  - after removing the broad recreate-hook path and keeping only the downloader fix that preserves forced init on nonzero seek segments, `live_real_stream_seek_resume_native_drm` passes again
  - but `cargo test -p kithara-integration-tests --test suite_heavy live_real_stream_fixed_seek_window_regression_drm -- --nocapture` still fails on `fixed_window_2_chunk_0`
- Exact new failure path:
  - target seek is `123.263139768s`
  - log shows `seek anchor path: decoder seek to segment start failed ... anchor_start=120s`
  - debug run with `RUST_LOG=kithara_hls::source_wait_range=debug,kithara_hls::source=debug,kithara_hls::downloader=debug,kithara_audio::pipeline::source=debug` captured the wait loop stalling on a stale metadata mapping
- Hard evidence of the stale-offset loop:
  - `/tmp/fixed_window_drm_debug.log` lines 969-975:
    - `range_start=24759749`
    - `total=24759749`
    - `num_entries=13`
    - `wait_range` requests `variant=3 segment_index=33`
    - downloader immediately reports `segment_index=33` as `already loaded, skipping`
  - this means the reader is exactly at the loaded watermark, but playlist metadata still maps that byte offset to the previous loaded segment instead of the next segment
- Refined root-cause hypothesis:
  - the remaining DRM live failure is in `wait_range` / `find_segment_at_offset`, not in blanket decoder recreation
  - decrypted committed sizes have already shortened the actual loaded layout, but unreconciled metadata for earlier segments on the same variant still leaves the next segment start too far to the right
  - result: `request_on_demand_segment` re-requests a segment that is already loaded but does not cover `range_start`, and the loop makes no forward progress

### 2026-03-10 10:28 MSK

- The previous `wait_range`-only hypothesis is now disproved as the sole root cause.
- New minimal proof came from a focused unit repro in `crates/kithara-audio/src/pipeline/source.rs`:
  - `retry_decode_failure_after_anchor_seek_reuses_anchor_offset_when_timeline_is_stale`
  - it failed with `left: 115 right: 128`
  - this showed post-seek retry was reusing the stale pre-seek `timeline.byte_position` instead of the explicit anchor boundary
- Stage-6 DRM live failure was a two-part audio recovery bug:
  - `pending_seek_recover_offset` was captured from mutable stream state after seek, not from the known anchor/recreate boundary
  - decode-forward retry from `anchor.byte_offset` reused the absolute target duration instead of the original relative in-segment skip, so retries could recreate correctly but then skip forward from the wrong origin
- Fix applied:
  - anchor-based seek paths now persist recover offset from the explicit boundary (`anchor.byte_offset` or `recreate_offset`)
  - decode-forward retry now persists and reuses the original skip relative to that recreate boundary
  - decode-forward recovery state now stays armed until the first visible PCM chunk lands; it is no longer cleared on fully dropped hidden chunks
- Verification:
  - `cargo test -p kithara-audio retry_decode_failure_after_flac_fmp4_anchor_skip_reuses_original_relative_target -- --nocapture`
  - `cargo test -p kithara-audio retry_decode_failure_after_anchor_seek_reuses_anchor_offset_when_timeline_is_stale -- --nocapture`
  - `cargo test -p kithara-audio apply_time_anchor_seek_arms_recovery_from_anchor_boundary_when_segment_range_lags -- --nocapture`
  - `cargo test -p kithara-audio retry_decode_failure_after_decode_forward_skip_recreates_without_seek_zero -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy live_real_stream_fixed_seek_window_regression_drm -- --nocapture`
  - all passed

### 2026-03-10 10:48 MSK

- New narrow HLS-only reproducer after the DRM fixed-window work:
  - `cargo test -p kithara-integration-tests --test suite_heavy live_ephemeral_revisit_sequence_regression_hls -- --nocapture`
  - initially failed at `repro_random_seek_0_chunk_0`
- First audio-only proof:
  - `crates/kithara-audio/src/pipeline/source.rs`
  - extending `retry_decode_failure_after_seek_recreate_preserves_original_target` showed `pending_seek_recover_target` was cleared before recreate and not restored on successful recreate paths
  - this let the first recreated decoder EOF terminate recovery too early
- That was real but not sufficient:
  - after restoring `pending_seek_recover_target`, the HLS repro no longer exited with immediate EOF
  - it instead looped on `decode EOF right after seek`
  - debug log: `/tmp/live_ephemeral_revisit_hls_after_target_fix.log`
- Next narrowed HLS-side proof:
  - new downloader unit repro:
    - `cargo test -p kithara-hls segment_loaded_for_demand_requires_init_after_nonzero_seek -- --nocapture`
  - it showed `segment_loaded_for_demand()` can incorrectly reuse a cached nonzero seek target without re-fetching `init`
  - fix applied: active seek target now refuses the `already loaded` fast-path when `force_init_for_seek` is still armed but the stitched segment lacks usable init bytes
- That HLS fix was also real but still not sufficient for the first random seek:
  - the first on-demand seek segment was now re-fetched with init correctly
  - but audio recovery still looped because recreated AAC/fMP4 anchor fast-path fell back to absolute `decoder.seek(anchor_start)` retries on the recreated decoder
- Final audio root cause for the HLS revisit loop:
  - new unit repro:
    - `cargo test -p kithara-audio recreate_decoder_for_seek_uses_anchor_fast_path_for_fmp4_retry -- --nocapture`
  - for AAC/fMP4 anchor fast-path, recreated decoders were not keeping `pending_decode_forward_epoch`
  - this sent follow-up retries through `decoder.seek(174s)` on the recreated decoder instead of staying in decode-forward mode from the recreate boundary
- Fix applied:
  - successful recreate paths now keep `pending_seek_recover_target` armed
  - AAC/fMP4 recreated anchor fast-path now keeps retries in decode-forward mode instead of re-entering absolute `decoder.seek()` on the recreated decoder
  - downloader now refuses to treat the active nonzero seek target as reusable when forced-init recovery still requires an init-bearing refetch
- Verification:
  - `cargo test -p kithara-audio retry_decode_failure_after_seek_recreate_preserves_original_target -- --nocapture`
  - `cargo test -p kithara-audio recreate_decoder_for_seek_uses_anchor_fast_path_for_fmp4_retry -- --nocapture`
  - `cargo test -p kithara-audio retry_decode_failure_after_decode_forward_skip_recreates_without_seek_zero -- --nocapture`
  - `cargo test -p kithara-hls segment_loaded_for_demand -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy live_ephemeral_revisit_sequence_regression_hls -- --nocapture`
  - all passed

### 2026-03-10 11:18 MSK

- New minimal DRM live repro is no longer order-only; it fails in isolation:
  - `cargo test -p kithara-integration-tests --test suite_heavy live_stress_real_stream_seek_read_cache_drm_ephemeral -- --nocapture`
  - before the new fixes it consistently failed with:
    - `stress seek underflow: expected at least 65 seek ops, got 44`
- First root cause was a same-variant stale-base recreate bug in audio seek recovery:
  - debug run showed `seek anchor alignment` on FLAC/fMP4 same-variant seeks recreating from `base_offset=0`
  - that forced the decoder to start again from segment 0 after every nonzero stale-base seek instead of from the target segment init
  - narrow proof:
    - `cargo test -p kithara-audio anchor_recreate_offset_keeps_same_variant_stale_seek_local -- --nocapture`
    - it failed with `left: 0 right: 7896633`
  - fix:
    - same-variant stale-base anchor recreate now stays local to `anchor.byte_offset` instead of falling back to the variant head
- Second root cause was init-shape loss on on-demand re-download:
  - debug log captured:
    - `commit_segment: replacing stale segment shape at current offset ... existing_init_len=623 incoming_init_len=0`
  - that meant a seek segment that was originally stitched as init+media could later be re-downloaded as media-only after eviction, which explains later `no suitable format reader found` during decoder recreate at that offset
  - fix:
    - `build_demand_plan()` now preserves `need_init=true` whenever the existing indexed segment shape already carries init bytes
  - narrow proof added:
    - `cargo test -p kithara-hls build_demand_plan_preserves_init_shape_for_redownloaded_seek_segment -- --nocapture`
- Third root cause was a transient stale seek anchor offset after DRM size reconciliation:
  - debug run for epoch 32 showed the same seek first resolving to `byte_offset=19286731`, then after the segment was actually reconciled/loading complete resolving again to `byte_offset=19286108`
  - the stale first offset caused a failed recreate/probe on the wrong byte boundary before retry could recover
  - fix:
    - after the first `decoder.seek(anchor_start)` failure, audio now refreshes the seek anchor and reuses the updated byte offset when the same segment/variant resolves to a corrected boundary
  - regression proof added:
    - `cargo test -p kithara-audio apply_time_anchor_seek_refreshes_anchor_after_failure -- --nocapture`
- Verification after the three fixes:
  - `cargo test -p kithara-audio anchor_recreate_offset_keeps_ -- --nocapture`
  - `cargo test -p kithara-audio apply_time_anchor_seek_refreshes_anchor_after_failure -- --nocapture`
  - `cargo test -p kithara-hls build_demand_plan_preserves_init_shape_for_redownloaded_seek_segment -- --nocapture`
  - `cargo test -p kithara-hls segment_loaded_for_demand_requires_init_after_nonzero_seek -- --nocapture`
  - `cargo test -p kithara-hls commit_segment_replaces_same_offset_entry_when_drm_shape_changes -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy live_stress_real_stream_seek_read_cache_drm_ephemeral -- --nocapture`
  - final narrow DRM live repro passed; the last run finished green at `80.25s`

### 2026-03-10 12:08 MSK

- Re-read the journal before continuing to avoid circling back to disproved branches.
- Confirmed current work is not a revisit of the banned paths:
  - not treating missing `serial` as the root cause
  - not returning to stale-request / fuzz issues
  - not reintroducing blanket decoder recreation
- New order-sensitive tail after the 11:18 fixes:
  - isolated `live_stress_real_stream_seek_read_cache_drm_ephemeral` was green again
  - but module run still failed with:
    - `stress seek underflow: expected at least 65 seek ops, got 63`
  - log comparison against the isolated green run showed the same seek/fallback pattern inside the test, just cut short by the phase budget
- New minimal order-sensitive reproducer for that tail:
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream -- --skip live_ephemeral_revisit_sequence_regression_hls --skip live_ephemeral_revisit_sequence_regression_drm --skip live_ephemeral_small_cache_playback_hls --skip live_ephemeral_small_cache_playback_drm --skip live_ephemeral_small_cache_seek_stress_hls --skip live_ephemeral_small_cache_seek_stress_drm --skip live_real_drm_playback_smoke --skip live_real_stream_fixed_seek_window_regression_hls --skip live_real_stream_random_seek_prefix_regression_hls --skip live_real_stream_seek_resume_native_hls --skip live_stress_real_stream_seek_read_cache_drm_mmap --skip live_stress_real_stream_seek_read_cache_hls_ephemeral --skip live_stress_real_stream_seek_read_cache_hls_mmap -- --nocapture`
  - before the new fix it failed reproducibly on:
    - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_drm_ephemeral`
    - `stress seek underflow: expected at least 65 seek ops, got 63`
- New root cause is lifecycle/shutdown, not another in-test offset corruption:
  - `crates/kithara-stream/src/backend.rs` documents the downloader worker as “joined on drop after cancellation”
  - actual `Backend::drop` only called `cancel.cancel()` and detached the worker thread without joining it
  - this left previous heavy DRM downloader threads alive long enough to steal progress budget from the next heavy live DRM test
- Narrow unit proof added:
  - `cargo test -p kithara-stream drop_waits_for_worker_exit -- --nocapture`
  - before the fix it failed with:
    - `backend drop must wait for the downloader worker to exit`
- Fix applied:
  - `Backend` now stores the worker handle as a real field and, on native targets, joins it during `Drop` after cancellation
- Verification after the lifecycle fix:
  - `cargo test -p kithara-stream drop_waits_for_worker_exit -- --nocapture`
  - `cargo test -p kithara-stream demand_must_not_wait_for_step -- --nocapture`
  - narrow DRM 2-test subset (`random_seek_prefix_regression_drm` + `seek_read_cache_drm_ephemeral`) no longer reproduced the underflow
  - narrow DRM 4-test subset (`fixed_seek_window_regression_drm`, `random_seek_prefix_regression_drm`, `seek_resume_native_drm`, `seek_read_cache_drm_ephemeral`) passed
  - full module verification:
    - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream -- --nocapture`
    - result: `17 passed; 0 failed`
- Updated conclusion:
  - remaining module-order DRM tail after the seek/offset fixes was caused by downloader worker lifetime leaking across tests
  - this was a new bug class and not a return to the older offset hypotheses

### 2026-03-10 12:40 MSK

- Re-read the journal before continuing; the next failures are a new cluster, not a return to the already disproved DRM-tail hypotheses above.
- New minimal standalone reproducer after the downloader/audio-worker shutdown fixes:
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::stress_chunk_integrity::stress_chunk_integrity_ephemeral -- --nocapture`
  - result: fails in isolation in about `4.5s`
  - panic:
    - `132 intra-chunk saw-tooth breaks in decoded data (>1 tolerance)`
- This means the remaining bug is no longer order-sensitive at the module level; there is a real standalone seek/alignment regression in synthetic WAV HLS.
- Key fresh evidence from `/tmp/stress_chunk_integrity_ephemeral_isolated.log`:
  - on nonzero variant-1 seeks, the demand-loaded target segment is committed with `init_len=44` semantics:
    - e.g. `segment_index=10 byte_offset=2000132 end=2200176`
    - and later `segment_index=15 byte_offset=3000176 end=3200220`
  - immediately before those commits, audio still takes the old path:
    - `seek anchor alignment: compare format current_codec=None target_codec=None current_variant=Some(0) target_variant=Some(1) codec_changed=false variant_changed=true stale_base_offset=false base_offset=0`
    - then `seek anchor path: decoder seek to segment start succeeded`
- Refined hypothesis:
  - the remaining synthetic HLS corruption is not another timeout-budget leak; it is a seek alignment bug where a nonzero WAV seek target carries a fresh init/header, but audio still trusts the old decoder via `decoder.seek(...)`
  - at the same time, `MediaInfo` has degraded to `codec=None/container=None`, so the seek path no longer has enough format information to choose the narrow recreate path
  - this matches the observed symptoms:
    - intra-chunk saw-tooth breaks
    - L/R channel mismatches in `stress_seek_lifecycle` / `stress_seek_abr_audio`
- Next step:
  - add a narrow unit repro in `crates/kithara-audio/src/pipeline/source.rs` for variant-switched nonzero WAV seek with missing target hints
  - fix the seek-path decision so it preserves/uses the known format hints and recreates the decoder for this header-bearing WAV seek case instead of reusing the stale decoder

### 2026-03-10 12:36 MSK

- Re-read the journal before continuing so the next step would not drift back to old DRM-offset hypotheses.
- New minimal reproducer is not order-sensitive and is outside the already-fixed `live_stress_real_stream` cluster:
  - `cargo test -p kithara-integration-tests --test suite_heavy stress_chunk_integrity_ephemeral -- --nocapture`
  - fails in isolation in `4.50s`
- Exact isolated failure:
  - `tests/tests/kithara_hls/stress_chunk_integrity.rs:609`
  - `132 intra-chunk saw-tooth breaks in decoded data (>1 tolerance)`
- Updated conclusion from this isolated repro:
  - the current remaining bug is no longer the previous heavy module lifecycle tail
  - the active failure class is PCM/HLS data-integrity corruption during seek/ABR/reset paths
  - because the failure is intra-chunk, the next inspection should prioritize frame/sample alignment and chunk assembly, not timeout thresholds or serial execution

### 2026-03-10 12:57 MSK

- Root cause for the new isolated PCM/HLS corruption cluster is not a new byte-offset math bug.
- Narrow proof came from the isolated repro plus seek-path logs:
  - `cargo test -p kithara-integration-tests --test suite_heavy stress_chunk_integrity_ephemeral -- --nocapture`
  - before the fix it failed with:
    - `132 intra-chunk saw-tooth breaks in decoded data (>1 tolerance)`
  - log showed repeated seeks falling back to stale decoder semantics on WAV/HLS while sparse HLS `MediaInfo` omitted codec/container fields
- Exact cause:
  - HLS `media_info()` can surface only partial metadata (for this WAV fixture it keeps `variant_index` but not `codec/container`)
  - several audio seek decision points consumed that sparse value directly instead of merging it with cached/user-provided hints
  - as a result the WAV-specific anchor recreate fast-path was not selected consistently, and seeks reused old decoder/parser state instead of recreating from the init-bearing anchor boundary
- Fix applied:
  - audio seek-path now resolves `MediaInfo` through a merged view (`shared_stream.media_info()` overlaid onto cached hints) before deciding:
    - anchor fast-path / WAV fast-path
    - retry preservation
    - decode-forward recovery
    - decoder recreation after seek failure
- Narrow verification:
  - `cargo test -p kithara-audio apply_anchor_seek_with_fallback_recreates_nonzero_wav_seek -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy stress_chunk_integrity_ephemeral -- --nocapture`
  - both passed
- Updated conclusion:
  - the active cluster is a sparse-`MediaInfo` / stale-WAV-parser seek bug in the audio pipeline
  - this is distinct from the earlier DRM offset reconciliation work and from the later worker-lifecycle leaks

### 2026-03-10 13:04 MSK

- Re-read the journal before continuing so the next step would not slip back into the disproved DRM-offset branch.
- The 12:57 fix was necessary but not sufficient by itself; there was an earlier sparse-`MediaInfo` loss at audio construction time:
  - `Audio::new()` built `initial_media_info` as `user_media_info.or_else(stream_media_info)`
  - for the synthetic WAV/HLS fixtures, the user hint carries `codec/container`, but the stream-side `MediaInfo` carries the current `variant_index`
  - that `or_else` dropped the stream variant whenever user hints were present, which explains the earlier log pattern:
    - `current_variant=None target_variant=Some(1)`
- Narrow proof added:
  - new unit test:
    - `cargo test -p kithara-audio resolve_initial_media_info_preserves_stream_variant_for_user_hints -- --nocapture`
  - after replacing the `or_else` with a field-wise merge, the isolated repro no longer showed the stale pattern
- Verification on the original standalone reproducer:
  - `cargo test -p kithara-audio apply_anchor_seek_with_fallback_recreates_nonzero_wav_seek -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::stress_chunk_integrity::stress_chunk_integrity_ephemeral -- --nocapture`
  - both passed
- Fresh log evidence from `/tmp/stress_chunk_integrity_ephemeral_after_initial_variant_fix.log`:
  - the bad signature is gone:
    - no more `current_variant=None`
  - nonzero WAV seeks now recreate from the init-bearing anchor boundary instead of trusting the old parser:
    - e.g. `Recreating decoder for new format ... variant_index: Some(1) ... base_offset=2000132`
    - later `Decoder recreated successfully ... base_offset=3000176`
- Updated conclusion:
  - the standalone WAV/HLS corruption was a two-stage sparse-`MediaInfo` bug:
    - seek-path decisions needed merged effective `MediaInfo`
    - and `Audio::new()` also needed to merge user hints with stream variant metadata at construction time
