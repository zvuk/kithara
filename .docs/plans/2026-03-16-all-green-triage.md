# All-Green Triage Plan

Date: 2026-03-16

## Rule Of Use

- Scope фиксируется заранее.
- В этом файле нельзя переписывать backlog под текущий симптом.
- Разрешённые изменения:
  - отметить задачу чекбоксом
  - добавить факт в `Evidence Log`
  - добавить verification к уже существующей задаче
- Если появляется новая проблема вне scope, она попадает в `Deferred`, а не меняет основной список задач.

## Goal

Вернуть workspace в `all green`, не ломая уже закрытые DRM/HLS regression guardrails.

## Guardrails

- `drm_stream_byte_integrity` должен оставаться зелёным на каждом шаге.
- `open_resource*` не создаёт ресурс.
- `acquire_resource*` является единственным create/write path.
- Для HLS seek demand должен резолвиться из decoder-landed offset и корректного variant authority.

## Fixed Tasks

- [x] Assets/resource lifecycle contract
  - Contract:
    - `open_resource*` only opens existing resources.
    - `acquire_resource*` is the only creation/mutation path.
    - dropping an uncommitted write handle must not leave a ghost resource.
  - Main files:
    - [lease.rs](/Users/litvinenko-pv/code/kithara/crates/kithara-assets/src/lease.rs)
    - [resource_path_test.rs](/Users/litvinenko-pv/code/kithara/tests/tests/kithara_assets/resource_path_test.rs)
    - [eviction_integration.rs](/Users/litvinenko-pv/code/kithara/tests/tests/kithara_assets/eviction_integration.rs)
    - [integration_storage_assets.rs](/Users/litvinenko-pv/code/kithara/tests/tests/kithara_assets/integration_storage_assets.rs)
    - [streaming_resources_comprehensive.rs](/Users/litvinenko-pv/code/kithara/tests/tests/kithara_assets/streaming_resources_comprehensive.rs)
    - [ephemeral.rs](/Users/litvinenko-pv/code/kithara/tests/tests/kithara_hls/ephemeral.rs)
  - Verification:
    - `cargo test -p kithara-assets --test resource_state -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_light kithara_assets::resource_path_test -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_light kithara_assets::eviction_integration::eviction_ignores_missing_index -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_light kithara_assets::integration_storage_assets::streaming_resource_concurrent_write_and_read_across_handles -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_light kithara_assets::streaming_resources_comprehensive::streaming_resource_commit_behavior_case_1 -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_light kithara_hls::ephemeral:: -- --nocapture`

- [x] HLS landed-offset / truncated reset-layout regression
  - Contract:
    - `commit_seek_landing(...)` must not trust truncated reset layout when decoder lands before `anchor.byte_offset`.
    - demand must resolve from landed byte offset using the anchor variant metadata when needed.
  - Main files:
    - [source.rs](/Users/litvinenko-pv/code/kithara/crates/kithara-hls/src/source.rs)
  - Verification:
    - `cargo test -p kithara-hls commit_seek_landing_uses_anchor_variant_metadata_when_reset_truncates_prefix --lib -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::source_internal_cases::commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`

- [x] HLS decoder-init contract on reset/recreate
  - Contract:
    - the first decoder-visible segment after cross-codec seek/reset must retain `init`
    - `init` must belong to the same variant as the media bytes handed to the decoder
    - `commit_segment(...)` must treat fetched `HlsFetch.init_len` as authoritative
  - Main files:
    - [downloader.rs](/Users/litvinenko-pv/code/kithara/crates/kithara-hls/src/downloader.rs)
  - Verification:
    - `cargo test -p kithara-hls commit_preserves_init_for_first_cross_codec_seek_target_segment --lib -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`

## Active Scope

- [ ] Re-run HLS stress/live cluster after the landed-offset fix
  - Target failures from `test.log`:
    - `kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm`
    - `kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_hls`
    - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm`
    - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_hls`
    - `kithara_hls::live_stress_real_stream::live_real_stream_random_seek_prefix_regression_drm`
    - `kithara_hls::live_stress_real_stream::live_real_stream_seek_resume_native_drm`
    - `kithara_hls::live_stress_real_stream::live_real_stream_seek_resume_native_hls`
    - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_drm_ephemeral`
    - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_drm_mmap`
    - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_hls_ephemeral`
    - `kithara_hls::stress_chunk_integrity::stress_chunk_integrity_ephemeral`
    - `kithara_hls::stress_chunk_integrity::stress_chunk_integrity_mmap`
    - `kithara_hls::stress_seek_abr::stress_seek_during_abr_switch_real_decoder_drm`
    - `kithara_hls::stress_seek_abr_audio::stress_seek_abr_audio`
    - `kithara_hls::stress_seek_audio::stress_seek_audio_hls_wav_ephemeral`
    - `kithara_hls::stress_seek_audio::stress_seek_audio_hls_wav_mmap`
    - `kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_ephemeral`
    - `kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap`
    - `multi_instance::concurrent_mixed::mixed_four_file_four_hls`
    - `multi_instance::concurrent_mixed::mixed_two_file_two_hls`
  - Expected output of this task:
    - split the remaining red tests into root-cause groups
    - identify which failures disappeared transitively after the landed-offset fix
    - pick the next single root-cause task
  - Mandatory guardrail:
    - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`

- [ ] Fix architecture seam #1: ABR target variant vs applied HLS layout
  - Why this exists:
    - `ABR` is the authority for the target variant.
    - `StreamIndex` is the authority for the applied byte layout.
    - seek / replay / reset paths still allow these two truths to drift.
  - Typical failure signatures:
    - `seek anchor resolution failed: ... seek offset not found: variant=3 segment=...`
  - Exit criteria:
    - seek-anchor resolution uses one consistent layout authority
    - no HLS seek path derives byte offsets from a variant that is not yet applied in layout

- [ ] Fix architecture seam #2: decoder-start contract after HLS seek/reset
  - Why this exists:
    - decoder recreate / exact seek must start from a self-contained byte boundary
    - `base_offset`, `format_change_segment_range()`, `init`, and media variant can still disagree
    - today `reuse current decoder session` vs `recreate decoder` is decided too early and too implicitly
    - the worker can start decoder initialization before decoder-start bytes are ready in the same coordinate system
  - Stable contract:
    - decoder always receives `init + media`
    - `init` belongs to the same variant as the media being decoded
    - recreate/seek uses the correct absolute stream boundary for that decoder-visible start
    - decoder initialization is an explicit FSM step:
      - `AwaitingDecoderInput`
      - `CreatingDecoder`
      - `SeekingDecoder`
      - `AwaitingDecoderOutput`
    - `same codec` does not imply `reuse current decoder session`
      - if the current session is stale/exhausted or its start boundary is no longer valid for the seek target, recreate is allowed
    - no broad fallback on arbitrary `decoder.seek()` errors
      - recovery is valid only for explicitly classified stale-session / wrong-boundary cases
  - Minimal FSM shape to introduce:
    - `SeekRequested`
      - classifies the seek and chooses decoder-start intent
    - `AwaitingDecoderInput`
      - owns `seek_epoch`, `seek target`, `media_info`, `base_offset`, and cause
      - asks readiness/demand only for `base_offset..window`
    - `CreatingDecoder`
      - runs only after `AwaitingDecoderInput` says decoder-start bytes are ready
    - `SeekingDecoder`
      - runs exact seek only on a fresh or validated decoder session
    - `AwaitingDecoderOutput`
      - remains downstream of a fully prepared decoder session, never of a half-prepared one
    - `Decoding`
      - steady-state playback only after decoder output is observed
  - Typical failure signatures:
    - `exact decoder seek failed: unexpected end of file`
    - `failed to recreate decoder ... no suitable format reader found`
    - `resource missing` during post-seek decode
  - Exit criteria:
    - decoder-start preparation is represented explicitly in the FSM, not as implicit side effects inside `apply_time_anchor_seek(...)`
    - decoder creation is impossible before `AwaitingDecoderInput` has observed ready bytes for the decoder-start boundary
    - readiness/demand for decoder recreation uses `recreate.offset` / decoder-start boundary, not `current position`
    - exact seek is never attempted on a decoder session whose start boundary is already known to be invalid for the target

- [ ] Fix architecture seam #3: materialization/progress contract from HLS to audio worker
  - Why this exists:
    - `StreamIndex` may say a segment is visible while the underlying resource is not yet readable
    - audio worker then waits forever for a chunk that never becomes decodable
  - Typical failure signatures:
    - `wait_range budget exceeded`
    - `next_chunk timeout ... (is_eof=false)`
    - hang-detector panics in `audio_worker` / `recv_outcome_blocking`
  - Exit criteria:
    - when a segment is visible and demanded, resources become readable in the same coordinate system the worker is waiting on
    - downstream timeouts disappear as a consequence, not by adding local retries/gates

- [x] Turn the MP3 app hang from `app.log` into a deterministic regression
  - Observed log sequence:
    - `SeekApplied`
    - `DecodeStarted`
    - `OutputCommitted`
    - `SeekComplete { position: 12.976054409s, seek_epoch: 1 }`
    - then hang detector panic in `kithara_audio::pipeline::audio_worker::run_shared_worker_loop`
  - Expected output of this task:
    - one deterministic test that reproduces the file/MP3 post-seek hang
    - one narrow suspect area in file/audio shared worker path
  - Verification:
    - `cargo test -p kithara-file file_source_demand_range_requests_downloader --lib -- --nocapture`

- [x] Fix the MP3 post-seek hang
  - Prerequisite:
    - the regression above exists first
  - Mandatory guardrail:
    - no regressions in HLS tests, especially `drm_stream_byte_integrity`
  - Verification:
    - `cargo test -p kithara-file file_source_demand_range_requests_downloader --lib -- --nocapture`
    - `cargo test -p kithara-integration-tests awaiting_resume_enters_waiting_for_source_and_submits_demand -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_heavy live_stress_real_mp3_seek_read_cache -- --nocapture`
    - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`

## Deferred

- [ ] Any new failures not present in `test.log` or `app.log`
  - These do not enter active scope until the current backlog item is closed.

## Evidence Log

- 2026-03-16: added a deterministic byte-level contract for HLS decoder-start boundaries instead of overloading `drm_stream_byte_integrity` with internal seek/recreate semantics.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy format_change_segment_range_reads_self_contained_bytes_from_reset_layout_floor -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - `format_change_segment_range_reads_self_contained_bytes_from_reset_layout_floor` is green and proves that a reset-layout decoder-start boundary can be validated with generic bytes: `read_at(start)` returns `[init][media]` in absolute coordinates
  - full `drm_stream_byte_integrity` still catches a real remaining failure in `drm_stream_byte_integrity_drm_disk_auto`
    - `wait_range budget exceeded: range=1671896..1737432`
  Interpretation:
  - the missing contract was real, but it is not sufficient to close the remaining heavy red cluster
  - the unresolved product bug is still in HLS materialization/progress, not in the new byte-contract test itself

- 2026-03-16: isolated audio seek paths are healthy with an ideal source and a `unimock`-backed decoder.
  Verification:
  - `cargo test -p kithara-integration-tests seek_anchor_recreates_decoder_when_codec_changes -- --nocapture`
  - `cargo test -p kithara-integration-tests seek_uses_exact_target_after_anchor_preparation_without_decoder_recreate -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity_hls_disk_auto -- --nocapture`
  Result:
  - same-codec anchor seek still produces the first chunk after seek without decoder recreation
  - cross-codec seek now also proves the first chunk arrives from the recreated decoder stream
  - isolated `drm_stream_byte_integrity_hls_disk_auto` remains green
  Interpretation:
  - the remaining heavy HLS hangs are not explained by the audio worker FSM alone when fed ideal input
  - the next productive isolation target is the HLS decoder-start boundary itself, not another fallback in audio

- 2026-03-16: `WaitingForSource(Recreation)` and `RecreatingDecoder` now wait and demand on `recreate.offset`, not on `current position`.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_light waiting_recreation_uses_recreate_offset_for_readiness -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_light recreating_decoder_waits_for_recreate_offset_before_factory -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_light seek_anchor_recreates_decoder_when_codec_changes -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_light same_codec_seek_with_stale_base_offset_does_not_recreate_decoder -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - both new audio-side regression tests are green
  - existing narrow seek regressions remain green
  - `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`
  - representative heavy is still red, but its failure moved:
    - from `seek_0_chunk_0`
    - to `seek_1_chunk_0`
    - with a concrete recreate failure:
      - `failed to recreate decoder ... no suitable format reader found`
      - `base_offset=19286336`
  Interpretation:
  - one concrete breach inside seams `#2/#3` is closed:
    - recreate-path readiness and demand no longer live in stale cursor coordinates
  - the remaining live failure is now later and narrower:
    - decoder recreation reaches the correct wait point
    - but the decoder-start boundary chosen at `base_offset=19286336` is still not self-contained/decodable

- 2026-03-16: post-switch HLS demand routing now follows the mixed applied layout once the read offset is already in the switched tail.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_uses_layout_owned_segment_in_switched_tail -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_uses_abr_target_after_midstream_switch -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_clamps_to_target_floor_after_midstream_switch -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity_hls_disk_auto -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - mixed-tail demand no longer reinterprets stream offsets against the pure target-variant timeline
  - once the switched tail is materialized, demand follows the `StreamIndex` layout owner for that offset
  - `drm_stream_byte_integrity_hls_disk_auto` is green again
  - full `drm_stream_byte_integrity` is green again with `12 passed / 0 failed`
  Interpretation:
  - one concrete seam inside architecture seam `#1` is now closed:
    - stale post-switch reroutes through the target-floor segment
    - instead of through the true layout-owned target segment for the requested offset
  - the remaining HLS-heavy failures should now be looked at as later seek/decode/materialization contracts, not this routing bug

- 2026-03-16: ported the first narrow regression from the parallel `fix/nextest-failures` worktree without importing its architecture.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_uses_abr_target_after_midstream_switch -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_uses_variant_fence_when_abr_hint_changes -- --nocapture`
  Result:
  - existing fence-only contract still holds:
    - `test_wait_range_uses_variant_fence_when_abr_hint_changes` is green
  - new post-switch contract is red:
    - `test_wait_range_uses_abr_target_after_midstream_switch` fails with `left: 0, right: 1`
  Interpretation:
  - the current branch still routes on-demand re-pushes through the stale pre-switch variant after `had_midstream_switch`
  - this confirms one concrete missing seam from the parallel worktree:
    - normal read fencing may still use `variant_fence`
    - but post-switch demand routing needs its own authority and cannot keep following the stale fence/layout prefix
- 2026-03-16: audited parallel worktree `fix/nextest-failures` (`0ccca6efed`), which reports `7 failed` in its local `test.log`.
  Verification:
  - `git worktree list --porcelain`
  - `git diff --stat 0b0d0f7ed..0ccca6efed`
  - `.worktrees/fix-nextest/test.log`
  - `git diff 0b0d0f7ed..0ccca6efed -- tests/tests/suite_heavy.rs tests/tests/kithara_hls/drm_stream_integrity.rs`
  Result:
  - the branch does pass several heavy HLS seek cases that are still red on the current branch
  - but its `7 failed` result is not directly comparable to the current guardrail set because it removes `mod drm_stream_integrity` from `suite_heavy`
  - it also weakens `drm_stream_integrity` by dropping the strict phase-1 full-stream assertions
  - the same branch reintroduces rejected architecture:
    - `download_state.rs`
    - `Timeline.total_bytes`
    - `variant_fence` as current-variant authority
    - `read_at -> Data(0)` on holes
  Interpretation:
  - this worktree is not cherry-pickable as a branch-level solution
  - the only plausible reusable signal is narrower:
    - on-demand HLS routing after midstream switch should follow ABR target variant
    - that routing likely needs a floor clamp to the applied switched layout

- 2026-03-16: `SeekLayout::Reset` no longer collapses HLS into reset-relative coordinates.
  Verification:
  - `cargo test -p kithara-hls reset_layout_keeps_absolute_total_length_for_seek_recreate --lib -- --nocapture`
  - `cargo test -p kithara-hls format_change_segment_range_uses_reset_layout_floor_when_no_segments_are_loaded --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - both HLS unit regressions are green
  - `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`
  - representative heavy no longer fails with `seek past EOF`; it now fails later with:
    - `failed to recreate decoder e=Backend(Unsupported("core (probe): no suitable format reader found"))`
    - `Failed to recreate decoder base_offset=1319645`
    - `next_chunk timeout at stage='seek_1_chunk_0'`
  Interpretation:
  - the absolute-vs-relative reset bug is closed
  - the next live blocker is now decoder-start validity, not HLS `len()/EOF`
  - `range_ready_from_segments(...)` / recreate readiness can still say `Ready` for a decoder-start boundary that does not produce a self-contained decodable stream for Symphonia
- 2026-03-16: `WaitContext::Recreation` and `RecreatingDecoder` now use decoder-start coordinates instead of `current position`.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_light waiting_recreation_ -- --nocapture`
  Result:
  - `waiting_recreation_uses_recreate_offset_for_readiness` is green
  - `waiting_recreation_demands_recreate_offset_not_current_cursor` is green
  Interpretation:
  - the old seam `#2/#3` breach where readiness and demand fell back to stale cursor coordinates is closed in the reduced model
- 2026-03-16: same-codec exact seek recovery is now boundary-driven, not a broad fallback.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_light same_codec_anchor_seek_recreates_decoder_after_unexpected_eof -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_light same_codec_anchor_seek_without_decoder_start_boundary_fails_track -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_light same_codec_seek_with_stale_base_offset_does_not_recreate_decoder -- --nocapture`
  Result:
  - recreate happens only when `format_change_segment_range()` provides an explicit decoder-start boundary and that boundary differs from the current `session.base_offset`
  - without an explicit boundary, exact seek still fails terminally
  - stale base-offset drift alone still does not trigger recreation
- 2026-03-16: representative heavy rerun after the decoder-start fixes still fails, but the failure is now more specific than `next_chunk timeout`.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`
  - `live_ephemeral_small_cache_seek_stress_drm` still fails, but now the decisive product signal is:
    - `failed to recreate decoder e=Backend(Unsupported("core (probe): no suitable format reader found"))`
    - `Failed to recreate decoder base_offset=0`
  Interpretation:
  - seam `#2` remains open because decoder-start boundary selection can still collapse to `base_offset=0`, which is not a valid self-contained start for this DRM seek path
  - the downstream `next_chunk timeout` remains a symptom, not the primary bug classification
- 2026-03-16: `HlsSource::format_change_segment_range()` now has a direct regression that proves reset layout floor must win over variant prefix metadata.
  Verification:
  - `cargo test -p kithara-hls format_change_segment_range_uses_reset_layout_floor_when_no_segments_are_loaded --lib -- --nocapture`
  - `cargo test -p kithara-hls format_change_segment_range_prefers_metadata_for_stale_init_segment_offset --lib -- --nocapture`
  Result:
  - new reset-layout-floor regression is green
  - stale init metadata regression remains green
- 2026-03-16: after fixing `format_change_segment_range()` to use the reset layout floor, the representative heavy DRM seek no longer recreates from `base_offset=0`; it now fails later with an absolute-vs-relative coordinate split.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`
  - `live_ephemeral_small_cache_seek_stress_drm` now logs:
    - `step_recreating_decoder: failed to seek stream ... seek past EOF: new_pos=19286336 len=7943120`
  Interpretation:
  - seam `#2` and seam `#1` now meet at one precise bug:
    - decoder-start `recreate.offset` is absolute
    - but the underlying reset layout still exposes a shorter reset-relative `len`
  - the next task should target absolute stream coordinates after `SeekLayout::Reset`, not decoder fallback logic
- 2026-03-16: seam `#2` is now treated as an explicit decoder-start FSM problem, not as a generic seek-fallback problem.
  Evidence:
  - `kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm` now fails with `seek anchor path: exact decoder seek failed err=SeekFailed("unexpected end of file")`
  - new red regression `kithara_audio::stream_source_tests::same_codec_anchor_seek_recreates_decoder_after_unexpected_eof` proves the current exact-seek path fails terminally instead of transitioning into a controlled decoder-start recovery
  Decision:
  - a broad helper that retries by recreating on any `decoder.seek()` error is **not** an accepted closure for seam `#2`
  - the acceptable direction is explicit FSM separation of:
    - decoder-start preparation
    - readiness/demand on decoder-start bytes
    - decoder recreation
    - exact seek on the recreated session
- 2026-03-16: the current FSM still checks `Recreation` readiness in the wrong coordinate system.
  Evidence:
  - `source_phase_for_wait_context(...)` and `submit_demand_for_current_state(...)` special-case `ApplySeek(Anchor)` but not `WaitContext::Recreation(...)`
  - `Recreation` therefore falls back to `shared_stream.phase()` / `shared_stream.position()` instead of `recreate.offset`
  Interpretation:
  - this is an architecture breach between seam `#2` and seam `#3`
  - even a correct recreate decision can stall because the worker waits and demands on the old cursor instead of the decoder-start boundary
- 2026-03-16: current worktree has additional uncommitted HLS seam `#1` fixes beyond the last `test.log`. The log with `20` failures is therefore a stale upper bound, not the current post-fix truth for the branch.
- 2026-03-16: new direct red regression now fixes the desired contract for future `AwaitingDecoderInput`.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_light same_codec_anchor_seek_waits_for_anchor_input_before_exact_seek -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_light recreating_decoder_transitions_to_waiting_with_decoder_input_demand -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - both transition regressions are green:
    - `ApplyingSeek -> WaitingForSource(ApplySeek)` emits demand for the anchor boundary immediately
    - `RecreatingDecoder -> WaitingForSource(Recreation)` emits demand for the decoder-start boundary immediately
  - `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`
  Interpretation:
  - `WaitingForSource(Recreation)` already existed as the logical decoder-input wait state
  - the real breach was not “missing state”, but missing immediate-demand behavior on transitions into the existing wait states
- 2026-03-16: added integration-level seam `#1` contract `kithara_hls::source_internal_cases::seek_reset_wait_range_uses_same_logical_segment_as_anchor`.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy seek_reset_wait_range_uses_same_logical_segment_as_anchor -- --nocapture`
  Result:
  - green
  - `seek_time_anchor -> reset_to(...) -> wait_range` already agree on the same `(variant, logical segment)` at the `HlsSource` boundary
  - remaining seam `#1` drift therefore sits deeper than source-side anchor/demand resolution
- 2026-03-16: tested the hypothesis that `plan_impl()` itself emits a new-epoch prefix batch below the reset-layout floor.
  Verification:
  - `cargo test -p kithara-hls plan_does_not_emit_prefix_batch_below_reset_layout_floor --lib -- --nocapture`
  Result:
  - green
  - this disproves `plan_impl()` as the primary seam `#1` root cause in the reduced model
- 2026-03-16: added deterministic downloader seam `#1` regressions:
  - `downloader::tests::invisible_prefix_fetch_after_reset_layout_does_not_advance_cursor`
  - `downloader::tests::invisible_prefix_segment_is_not_treated_as_next_planned_segment`
  Product fix:
  - `commit(...)` now advances the sequential cursor only for segments that are visible in the applied `StreamIndex` layout
  - `should_skip_planned_segment(...)` no longer treats a layout-invisible prefix segment as skippable just because its resource exists
  Verification:
  - `cargo test -p kithara-hls invisible_prefix --lib -- --nocapture`
  - `cargo test -p kithara-hls plan_does_not_emit_prefix_batch_below_reset_layout_floor --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy seek_reset_wait_range_uses_same_logical_segment_as_anchor -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - both downloader regressions are green
  - the planner hypothesis remains disproved
  - `drm_stream_byte_integrity` is back to `12 passed / 0 failed`
- 2026-03-16: representative rerun after the current seam `#1` work shows the active failures have moved deeper:
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::stress_seek_audio::stress_seek_audio_hls_wav_ephemeral -- --nocapture`
  Result:
  - none of these representative tests fail first on `seek offset not found`
  - `live_ephemeral_revisit_sequence_regression_drm` now fails in seam `#2` with `seek anchor path: exact decoder seek failed err=SeekFailed("unexpected end of file")`
  - `live_ephemeral_small_cache_seek_stress_drm` fails in seam `#3` as `next_chunk timeout at stage='seek_0_chunk_0' (is_eof=false)`
  - `stress_seek_audio_hls_wav_ephemeral` fails in seam `#3` via hang-detector in `audio_worker::run_shared_worker_loop` / `recv_outcome_blocking`
  - this does not yet close seam `#1` globally, but it is enough to make seam `#2` and seam `#3` the current execution priority
- 2026-03-16: refreshed `test.log` still contains the same 20 unique red tests from the HLS stress/live cluster; there is no net progress on the active backlog item yet.
- 2026-03-16: committed and then amended the decoder-init contract fix as clean checkpoint `025797a9c` (`fix(hls): preserve fetched init for decoder resets`). Verification:
  - `cargo test -p kithara-hls --lib --no-run`
  - `cargo test -p kithara-hls commit_preserves_init_for_first_cross_codec_seek_target_segment --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
- 2026-03-16: exploratory seek/reset changes were quarantined again into `stash@{0}` (`wip-hls-seek-reset-exploration-after-init-checkpoint`). Current branch is intentionally clean except for the committed checkpoints.
- 2026-03-16: refreshed failure signatures from the latest `test.log` now point to three architecture seams rather than one symptom bucket:
  - seek-anchor resolution / layout ownership mismatch:
    - `seek anchor resolution failed: ... seek offset not found: variant=3 segment=13`
    - `seek anchor resolution failed: ... seek offset not found: variant=3 segment=26`
  - decoder handoff after seek / recreate:
    - `seek anchor path: exact decoder seek failed err=SeekFailed("unexpected end of file")`
    - `failed to recreate decoder e=Backend(Unsupported("core (probe): no suitable format reader found"))`
    - `decode error e=Backend(IoError(Custom { kind: Other, error: "source error: Assets error: io error: resource missing" }))`
  - downstream no-progress symptoms after the handoff breaks:
    - `next_chunk timeout at stage='...' (is_eof=false)`
    - `wait_range budget exceeded`
    - hang-detector panics in `kithara_audio::pipeline::audio_worker::run_shared_worker_loop`
      and `kithara_audio::pipeline::audio::recv_outcome_blocking`
- 2026-03-16: committed safe checkpoint `b37c24ca4` (`fix(assets,hls): checkpoint lifecycle and landed seek fixes`) after re-running:
  - `cargo test -p kithara-assets --test resource_state -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::source_internal_cases::commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Exploratory follow-up changes remain quarantined in `stash@{0}` until they are split into verified fixes.
- 2026-03-16: restored only the file-backed MP3 wakeup fix from `stash@{0}` into [session.rs](/Users/litvinenko-pv/code/kithara/crates/kithara-file/src/session.rs). Verification:
  - `cargo test -p kithara-file file_source_demand_range_requests_downloader --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy live_stress_real_mp3_seek_read_cache -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
- 2026-03-16: `drm_stream_byte_integrity` is green with `12 passed / 0 failed`.
- 2026-03-16: direct red test `kithara_hls::source_internal_cases::commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant` was fixed by preventing `commit_seek_landing(...)` from trusting reset-truncated layout for pre-anchor landed offsets.
- 2026-03-16: current rerun of `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` was started but intentionally interrupted by user before completion.
- 2026-03-16: direct red test `kithara_file::session::tests::file_source_demand_range_requests_downloader` now fails deterministically. This matches the `app.log` post-seek hang shape: audio transitions into `WaitingForSource`, calls `Source::demand_range(...)`, and `FileSource` currently drops that signal instead of waking the downloader.
- 2026-03-16: `FileSource::demand_range(...)` now forwards to `FileCoord::request_range(...)`. The direct file regression is green, `live_stress_real_mp3_seek_read_cache` is green, and `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`.
- 2026-03-16: refreshed `test.log` contains 20 unique red tests, all still inside the HLS stress/live cluster. The concrete signatures split into three buckets:
  - `seek anchor resolution failed: ... seek offset not found: variant=3 segment=...` in `live_ephemeral_revisit_sequence_regression_{drm,hls}` and `live_ephemeral_small_cache_seek_stress_drm`
  - `seek anchor path: exact decoder seek failed err=SeekFailed("unexpected end of file")` in `live_stress_real_stream_seek_read_cache_{drm_ephemeral,drm_mmap,hls_ephemeral}`
  - `wait_range budget exceeded` / hang-detector timeouts in `live_real_stream_seek_resume_native_hls`, the `stress_seek_*` family, `stress_chunk_integrity::*`, and `multi_instance::concurrent_mixed::*`
- 2026-03-16: added unit regression `source::tests::seek_time_anchor_uses_layout_offset_when_abr_target_outpaces_layout`. Root cause: `resolve_seek_anchor()` used the ABR target variant to choose the segment, but then required target-variant metadata for byte offset even when the current `StreamIndex` layout still belonged to the old variant.
- 2026-03-16: `resolve_seek_anchor()` now keeps `variant_index` from ABR but resolves `byte_offset` through the current composed layout first. Verification:
  - `cargo test -p kithara-hls seek_time_anchor_uses_layout_offset_when_abr_target_outpaces_layout --lib -- --nocapture`
  - `cargo test -p kithara-hls commit_seek_landing_uses_anchor_variant_metadata_when_reset_truncates_prefix --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::source_internal_cases::commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
- 2026-03-16: after the seek-anchor offset fix, `kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_hls` is green again. `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` no longer reproduces `seek offset not found`; it now fails later as `next_chunk timeout at stage='seek_0_chunk_0'`, which moves that scenario from the anchor-resolution bucket into the hang/wait-range bucket.
- 2026-03-16: added red unit `source::tests::reset_layout_preserves_absolute_stream_coordinates`. Root cause: `StreamIndex::reset_to(...)` truncated the layout prefix and also collapsed `byte_map`/`effective_total` into tail-relative coordinates, while audio recreate still seeks in absolute stream offsets.
- 2026-03-16: `StreamIndex` now keeps the absolute offset of the first segment in the current reset layout, so `len()` and committed segment ranges stay in global stream coordinates after `SeekLayout::Reset`. Verification:
  - `cargo test -p kithara-hls reset_layout_preserves_absolute_stream_coordinates --lib -- --nocapture`
  - `cargo test -p kithara-hls --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
- 2026-03-16: after the absolute-coordinate reset fix, `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` no longer fails with `seek past EOF`. The current remaining symptom is still `next_chunk timeout at stage='seek_0_chunk_0'`, so the active root-cause bucket remains the HLS hang/wait-range path after seek.
- 2026-03-16: clean-base rerun after checkpoint split disproved the earlier intermediate observation above: `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` still fails first on `seek anchor resolution failed: ... seek offset not found: variant=3 segment=26`. This means part of the earlier anchor-resolution work lived only in quarantined exploratory changes, not in the committed checkpoint.
- 2026-03-16: added unit regression `downloader::tests::commit_preserves_init_for_first_cross_codec_seek_target_segment`. Root cause: `build_demand_plan()` could request `init` for the first decoder-visible segment after a cross-codec seek reset, but `commit_segment()` re-derived inclusion from `is_variant_switch || is_midstream_switch || segment_index == 0` and dropped fetched init when reset layout already pointed at the target variant.
- 2026-03-16: `commit_segment()` now trusts fetched `HlsFetch.init_len` as the authority for init inclusion. Verification:
  - `cargo test -p kithara-hls commit_preserves_init_for_first_cross_codec_seek_target_segment --lib -- --nocapture`
  - `cargo test -p kithara-hls build_batch_plans_same_codec_variant_change_keeps_metadata_layout --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  This fixed the decoder-init contract without breaking the DRM integrity guardrail, but `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` still fails in the hang bucket as `next_chunk timeout at stage='seek_1_chunk_0'`.
- 2026-03-16: after quarantining exploratory seek/reset patches into `stash@{0}` and amending the safe init-contract commit, the branch is clean again at `025797a9c`. Verification on clean base:
  - `cargo test -p kithara-hls --lib --no-run`
  - `cargo test -p kithara-hls commit_preserves_init_for_first_cross_codec_seek_target_segment --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
- 2026-03-16: refreshed clean `test.log` still shows `20` HLS-heavy failures. Stable signatures now cluster at architecture seams, not just per-test names:
  - `seek anchor resolution failed: ... seek offset not found: variant=3 segment=13|26`
  - `seek anchor path: exact decoder seek failed err=SeekFailed("unexpected end of file")`
  - source-readiness failures: `wait_range budget exceeded` and `Assets error: io error: resource missing`
  - downstream worker starvation symptoms: `next_chunk timeout ...` and hang-detector panics in `audio_worker::run_shared_worker_loop` / `recv_outcome_blocking`
- 2026-03-16: added unit regression `source::tests::seek_time_anchor_uses_applied_layout_offset_when_abr_target_outpaces_layout`. Root cause: `seek_time_anchor()` selected the logical segment using the ABR target variant, but still required target-variant metadata to recover the byte offset even when the current `StreamIndex` layout already implied the correct absolute position.
- 2026-03-16: `HlsSource::byte_offset_for_segment(...)` now falls back to the applied layout via `StreamIndex::layout_offset_for_segment(...)` before giving up on seek-anchor resolution. Verification:
  - `cargo test -p kithara-hls seek_time_anchor_uses_applied_layout_offset_when_abr_target_outpaces_layout --lib -- --nocapture`
  - `cargo test -p kithara-hls resolve_current_variant_uses_abr_atomic_not_variant_fence --lib -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_hls -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - `live_ephemeral_revisit_sequence_regression_hls` is green again
  - `live_ephemeral_small_cache_seek_stress_drm` no longer fails first on `seek offset not found`; it now fails later as `next_chunk timeout at stage='seek_0_chunk_0'`
  - seam `#1` is partially closed, and the active blocker in this representative DRM case has moved to seam `#3`
- 2026-03-16: debug rerun of `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` shows a stronger architecture breach than a plain timeout:
  - `seek plan: Reset — variant=3 segment_index=26 byte_offset=1319645`
  - then downloader fetch/commit goes to `variant=3 segment_index=0 current_segment_index=1`
  - hang happens before the first `seek_0_chunk_0` chunk
  This means the post-reset decoder recreate path is still steering to the variant's init-bearing prefix segment (`segment 0`) instead of the decoder-visible target start for the applied reset layout.
- 2026-03-16: current leading hypothesis for seam `#2/#3` is no longer “generic wait_range timeout”. It is a contract breach between:
  - `SeekLayout::Reset`, which redefines the applied layout around the target tail
  - `format_change_segment_range()` / recreate-decoder start offset, which still resolves from variant-prefix init semantics
  - `read_at()/phase_at()`, which only consider decoder-visible layout in `StreamIndex`
  The next step must be a regression test for this boundary, not another timeout-specific fix.
- 2026-03-16: added red unit regression `downloader::tests::invisible_prefix_fetch_after_reset_layout_does_not_advance_cursor`.
  It proves a concrete illegal transition in downloader state after source-side seek reset:
  - `StreamIndex::reset_to(target_segment, target_variant)` has already moved the applied layout to the seek tail
  - an invisible pre-seek prefix fetch for `(target_variant, segment 0)` is still allowed to commit
  - `commit()` then advances the sequential cursor from `0` to `1` even though that fetch is outside the applied reset layout
  Failure:
  - `left: 1`
  - `right: 0`
  This is the first deterministic proof that the remaining hang is rooted in downloader state drift, not in a generic timeout primitive.
- 2026-03-16: added red regression `kithara_audio::stream_source_tests::awaiting_resume_transitions_to_waiting_for_source_and_demands_landed_offset`. Root cause: after seek was already applied, `AwaitingResume` could block on `SourcePhase::Waiting*` without ever entering `WaitingForSource`, so the worker never called `submit_demand_for_current_state()` for the landed byte offset.
- 2026-03-16: `AwaitingResume` now uses a dedicated wait context and round-trips through `WaitingForSource` when the landed offset is not ready yet. Verification:
  - `cargo test -p kithara-integration-tests awaiting_resume_transitions_to_waiting_for_source_and_demands_landed_offset -- --nocapture`
  - `cargo test -p kithara-integration-tests seek_anchor_commits_actual_landed_offset_to_source -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - the FSM gap is closed at unit/integration-test level
  - `drm_stream_byte_integrity` remains green with `12 passed / 0 failed`
  - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm` is still red with the same `next_chunk timeout at stage='seek_0_chunk_0'`, so this fix is only a partial closure inside seam `#3`, not the root-cause fix for the representative heavy case
- 2026-03-16: seam `#3` exploratory audio-worker changes were quarantined into `stash@{0}` (`wip-audio-seam3-resume-waiting`) so the active worktree again reflects only seam `#1` HLS changes plus the new downloader regression.
- 2026-03-16: added integration-level seam `#1` contract `kithara_hls::source_internal_cases::seek_reset_wait_range_uses_same_logical_segment_as_anchor`.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy seek_reset_wait_range_uses_same_logical_segment_as_anchor -- --nocapture`
  Result:
  - green
  - `seek_time_anchor -> reset_to(...) -> wait_range` already agree on the same `(variant, logical segment)` at the `HlsSource` boundary
  - remaining deterministic seam `#1` failure therefore sits deeper, in downloader state/commit progression rather than in source-side anchor or demand resolution
- 2026-03-16: current uncommitted HLS seam `#1` work no longer preserves the DRM integrity guardrail on disk-backed auto DRM.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - `11 passed / 1 failed`
  - only `kithara_hls::drm_stream_integrity::drm_stream_byte_integrity_drm_disk_auto` fails
  - failure shape: `Phase 1 ended with read error before EOF: Timeout: wait_range budget exceeded: range=2447027..2512563`
  - `DRM-eph-auto` stays green on the same worktree
  This does **not** yet prove a new causal regression from the latest test-only changes: the same seam `#1` code path had an earlier green guardrail run. What it proves is that the current worktree is not reliably safe/commit-ready until `DRM-disk-auto` is made stable again.
- 2026-03-16: tested the hypothesis that `plan_impl()` itself emits a new-epoch prefix batch below the reset-layout floor.
  Verification:
  - `cargo test -p kithara-hls plan_does_not_emit_prefix_batch_below_reset_layout_floor --lib -- --nocapture`
  Result:
  - green
  - in the reduced model, `seek_time_anchor/reset_to(...)` plus a new timeline seek epoch are **not enough** to make `plan()` speculatively emit `segment 0` of the target variant
  - this disproves `plan_impl()` as the primary root cause in the minimal case and narrows the remaining illegal transition to acceptance/commit of already-issued or otherwise stale prefix fetches
- 2026-03-16: integrated only the narrow post-switch demand-routing seam from the parallel `fix/nextest-failures` branch.
  Verification:
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_uses_abr_target_after_midstream_switch -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_clamps_to_target_floor_after_midstream_switch -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_midstream_switch_target_request_stays_stable_until_ready -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy test_wait_range_clears_midstream_switch_after_target_range_becomes_ready -- --nocapture`
  - `cargo test -p kithara-integration-tests --test suite_heavy drm_stream_byte_integrity -- --nocapture`
  Result:
  - the new HLS seam tests are green:
    - post-switch re-push follows ABR target variant instead of stale `variant_fence`
    - target demand is clamped to the first materialized segment of the switched variant
    - the post-switch target request remains stable until `WaitOutcome::Ready`
    - `had_midstream_switch` is now one-shot and clears on the first `Ready`
  - this also removed the replay regression that briefly broke both `drm_disk_auto` and `hls_disk_auto` phase 2 scans from `pos=0`
  - `drm_stream_byte_integrity` is still **not** fully green on this worktree:
    - `11 passed / 1 failed`
    - the only remaining guardrail failure is `kithara_hls::drm_stream_integrity::drm_stream_byte_integrity_hls_disk_auto`
    - new failure shape:
      - `Phase 1 ended with read error before EOF: source error: Timeout: wait_range budget exceeded: range=1671896..1737432 elapsed=10.022318791s timeout=10s`
  Interpretation:
  - the stale post-switch demand-routing seam is now closed
  - the remaining blocker in the guardrail is no longer replay/reseek routing; it is a phase-1 materialization/progress failure on `HLS-disk-auto`
