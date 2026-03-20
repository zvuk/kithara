# 2026-03-13 HLS Seek Debug Journal

Append-only journal for the current investigation.

Rules for this file:
- New notes are appended only.
- Existing notes are not edited in place.
- Before adding a new entry, reread the whole file.

## Entry 001
Time: 2026-03-13 Europe/Moscow

Goal:
- Remove decoder fallback/recovery.
- Use integration tests as the main oracle.
- Find the real cause of HLS seek stalls/hangs instead of masking them.

What was changed first:
- Removed seek/decode fallback and recovery paths from `crates/kithara-audio/src/pipeline/source.rs`.
- Removed recovery-oriented tests from `tests/tests/kithara_audio/stream_source_tests.rs`.
- Kept decoder recreation only for codec change / format-boundary paths.

Immediate result:
- `cargo check -p kithara-audio` passed.
- Integration test `kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap` failed.
- Failure mode after removing recovery was not a decode error; it was a timeout/hang in the integration path.

Key observation:
- After same-codec ABR switch, `symphonia.seek(time)` often lands before the HLS anchor byte offset.
- Example from logs:
  - seek target anchor: `variant=1 segment_index=22 byte_offset=4400044/4400088`
  - actual landed stream position: `4267052`
- This means the virtual `Read + Seek` stream must support the landed offset correctly; the anchor alone is not enough.

## Entry 002
Time: 2026-03-13 Europe/Moscow

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy multi_instance::concurrent_mixed::mixed_two_file_two_hls -- --nocapture`

Result:
- The mixed multi-instance integration test passed.

Interpretation:
- Removing recovery did not break all HLS/integration paths.
- The main failure is narrower: aggressive seek lifecycle after ABR switching.

## Entry 003
Time: 2026-03-13 Europe/Moscow

Hypothesis:
- Part of the regression came from readiness / wake behavior rather than decoder recreation policy.

What was changed:
- Restored `wake_worker()` before blocking receive in `crates/kithara-audio/src/pipeline/audio.rs`.
- Changed `StreamAudioSource::source_is_ready()` back toward range-based readiness instead of `pos..pos+1`.

Result:
- The failure changed from a plain test timeout to a fast `HangDetector` panic in `recv_outcome_blocking`.
- This made the underlying issue easier to see.

Key observation from logs:
- `wait_range()` spun on `range_start=4267052`.
- The downloader was not fetching the segment that actually covered the landed offset.

Interpretation:
- The problem is in the seek/virtual-stream contract after ABR switch, not in decoder fallback removal itself.

## Entry 004
Time: 2026-03-13 Europe/Moscow

What was found:
- `committed_segment_for_offset()` in `crates/kithara-hls/src/source.rs` ignored variant boundaries in one path and also used a `last_of_variant + 1` heuristic.
- On a same-codec ABR switch, this caused the source to derive a wrong on-demand segment index from committed state instead of from metadata for the landed offset.

Concrete reproduction:
- After switch to `variant=1`, seek landed at `stream_pos=4267052`.
- Metadata for that offset points to about `segment_index=21`.
- The committed-lookup path instead produced `segment_index=10`.

What was changed:
- Added a variant check to the direct committed lookup.

Result:
- The failure moved but did not disappear.
- The queue still ended up processing many wrong or stale requests.

Interpretation:
- The variant filter was necessary but not sufficient.

## Entry 005
Time: 2026-03-13 Europe/Moscow

What was investigated next:
- Added temporary debug logs around `queue_segment_request_for_offset()` to see which branch selected the segment index.

What the logs showed:
- Early in the failure, `queue_segment_request_for_offset()` selected `committed lookup` with `segment_index=10` for `range_start=4267052`.
- Later, after more state evolved, the same function correctly selected `metadata lookup` with `segment_index=21`.
- Despite that, the downloader kept processing many old queued requests for `segment_index=10`.

Important conclusion:
- There are two separate issues:
  1. `committed_segment_for_offset()` is too aggressive after ABR switch; it must not use `last_of_variant + 1` for arbitrary seek-landed offsets.
  2. Worker-side polling / `is_range_ready()` floods `SegQueue` with repeated requests and lets stale/wrong requests dominate long enough to trigger the hang detector.

## Entry 006
Time: 2026-03-13 Europe/Moscow

What was changed:
- Removed the temporary debug logs from `queue_segment_request_for_offset()`.
- Removed the `last_of_variant + 1` heuristic from `committed_segment_for_offset()`.

Result:
- The original `segment_index=10` misrouting was eliminated.
- The integration test still failed, but now at a later seek epoch.
- New failure shape:
  - `step_awaiting_resume` kept reporting `source not ready`.
  - The downloader kept processing many duplicate requests for an already-loaded segment (for example `segment_index=37`) while the real needed segment (`segment_index=38`) was only being requested from the read path.
  - `recv_outcome_blocking` still tripped the hang detector after 1 second.

Current hypothesis:
- The remaining root cause is queue flooding / missing dedupe for on-demand segment requests in the worker-side polling path.
- `SegQueue` is append-only FIFO here, so repeated blocked polls can bury the current correct request behind a large number of stale or redundant requests.

Current next step:
- Add a proper dedupe / pending-request mechanism for HLS on-demand segment requests, or otherwise stop the worker-side polling path from flooding `segment_requests`.

## Entry 007
Time: 2026-03-13 Europe/Moscow

What was changed:
- Added a pending-request dedupe mechanism for HLS on-demand segment requests in `crates/kithara-hls/src/source.rs` and `crates/kithara-hls/src/downloader.rs`.
- Cleared pending requests on stale/skip/commit paths and on seek-epoch resets.

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`

Result:
- The test still failed, but the failure mode changed again.
- `HangDetector` no longer dominated the run; instead the test timed out after 10s.
- The key early log remains the same: after a same-codec ABR seek targeting anchor `variant=1 segment_index=22 byte_offset=4400088`, the decoder landed at `stream_pos=4267052`.
- With dedupe in place, the queue no longer appears to be the primary blocker.

New observation:
- The real inconsistency is now clearer: seek planning is anchored to segment 22, but the virtual stream seen by Symphonia can land inside the previous segment.
- Example from this run:
  - seek target anchor: `segment_index=22 byte_offset=4400088`
  - actual landed stream position: `4267052`
  - later successful range snapshots show this landed offset belongs to the previous segment window, not the anchor segment.
- This means the source/download state must derive post-seek demand from the landed offset, not continue trusting the anchor segment index after `decoder.seek()` returns.

Updated hypothesis:
- Queue flooding was a secondary issue and dedupe was still worth keeping.
- The remaining root cause is a mismatch between `seek_time_anchor()` planning and the actual landed byte position after Symphonia seek on the virtual HLS stream.
- The next fix should focus on post-seek segment/range reconciliation from the actual landed offset, while preserving the rule that same-codec ABR switches must not recreate the decoder.

## Entry 008
Time: 2026-03-13 Europe/Moscow

What was changed:
- Added a source-level contract test in `tests/tests/kithara_hls/source_internal_cases.rs` asserting that `Source::seek_time_anchor()` must not eagerly commit `Timeline.byte_position` before decoder landing is known.
- Added a stream-level unit test in `crates/kithara-stream/src/stream.rs` asserting that `Stream::seek_time_anchor()` must not move stream position by itself.
- Added an audio-layer test in `tests/tests/kithara_audio/stream_source_tests.rs` asserting that anchor-resolution failure must fail the track instead of falling back to direct seek.
- Removed eager byte-position commit from `crates/kithara-hls/src/source.rs` and `crates/kithara-stream/src/stream.rs`.
- Removed the HLS anchor-resolution -> direct-seek fallback from `crates/kithara-audio/src/pipeline/source.rs`.

What was tested:
- `cargo test -p kithara-stream seek_time_anchor_does_not_move_position -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light seek_anchor_resolution_failure_fails_track_without_direct_seek_fallback -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy seek_time_anchor_does_not_commit_reader_position_before_decoder_lands -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`

Result:
- All three new contract tests passed.
- The heavy stress test still failed by 10s timeout.
- The failure is now cleaner: the system no longer relies on eager anchor commit or anchor-resolution fallback to keep moving.

Key observation from the new stress run:
- The same fundamental mismatch remains.
- Example from the run:
  - seek target anchor: `segment_index=22 byte_offset=4400044`
  - actual landed stream position after decoder seek: `4267052`
  - observed current range after seek: `4200044..4400044`
- This confirms that removing eager anchor commit was correct, but not sufficient. The next missing piece is explicit post-seek reconciliation from actual landed offset back into HLS state.

Updated hypothesis:
- The remaining root cause is not queue dedupe and not anchor fallback.
- The remaining root cause is that the audio/HLS boundary still lacks an explicit `commit_seek_landing(...)` step that reconciles the actual landed byte position after `decoder.seek(...)`.

## Entry 009
Time: 2026-03-13 Europe/Moscow

What was changed:
- Added a new `Source::commit_seek_landing(...)` hook in `crates/kithara-stream/src/source.rs`.
- Added a source-level contract test in `tests/tests/kithara_hls/source_internal_cases.rs` asserting that post-seek demand must follow the actual landed segment, not the anchor segment.
- Added an audio-layer contract test in `tests/tests/kithara_audio/stream_source_tests.rs` asserting that `StreamAudioSource` must call the source reconciliation hook after `decoder.seek(...)`.
- Implemented HLS landed-offset reconciliation in `crates/kithara-hls/src/source.rs`:
  - resolve the landed segment from the authoritative post-seek byte position
  - update `current_segment_index`
  - clear queued speculative requests
  - enqueue demand for the landed segment
- Updated `crates/kithara-audio/src/pipeline/source.rs` so successful decoder seeks call `commit_seek_landing(...)` before moving the FSM to `AwaitingResume`.

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy commit_seek_landing_replaces_anchor_request_with_landed_segment -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light seek_anchor_commits_actual_landed_offset_to_source -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`

Result:
- Both new contract tests passed after the implementation.
- The heavy stress test still failed by 10s timeout.

Key observation from the new stress run:
- The original landed-offset mismatch is now fixed in the logs.
- Example from the run:
  - seek target anchor: `segment_index=20 byte_offset=4000044`
  - actual landed stream position after decoder seek: `3879980`
  - `commit_seek_landing(...)` reconciled HLS state to `segment_index=19`
- Similar reconciliations happened repeatedly (`5 -> 4`, `2 -> 1`, `31 -> 30`), so the source is no longer stuck on the speculative anchor segment after seek.

New observation:
- Each seek now appears to generate two pieces of demand work:
  1. speculative anchor demand from `apply_seek_plan()`
  2. authoritative landed-segment demand from `commit_seek_landing(...)`
- In the logs this shows up as the downloader first processing the anchor request, then immediately processing the landed-segment request.

Updated hypothesis:
- The correctness bug around landed-offset reconciliation is fixed.
- The remaining timeout is likely the next layer of architectural debt: `apply_seek_plan()` still speculatively enqueues anchor demand before the decoder lands.
- That speculative anchor request is now redundant and likely the main throughput cost in the 2000-seek stress test.
- The next refactor step should be to stop queueing anchor demand during planning and let `commit_seek_landing(...)` become the single authoritative place that issues post-seek demand.

## Entry 010
Time: 2026-03-13 Europe/Moscow

What was changed:
- Added downloader regressions in `crates/kithara-hls/src/downloader.rs` proving that a same-codec variant change must not request init data for either demand or batch plans.
- Changed HLS downloader init policy so variant-switch init insertion happens only for cross-codec transitions (or explicit seek-forced init), not for same-codec ABR switches.
- Added `DecodeError` regressions in `crates/kithara-decode/src/error.rs` proving that backend-wrapped `seek pending` must be treated as an interrupt signal, not a hard decode failure.

## Entry 011
Time: 2026-03-13 Europe/Moscow

What was retested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_chunk_integrity::stress_chunk_integrity_ephemeral -- --nocapture`

Result:
- The failure reproduces quickly and cleanly.
- The test still panics with `10 direction errors (expected descending after ABR switch)`.

Key observations from the current log:
- `suite_light` is already green, so the remaining regression is concentrated in heavy HLS runtime.
- The repro is a same-codec ABR switch:
  - `ABR variant switch from=0 to=1 cross_codec=false reason=DownSwitch`
- Despite that, runtime still downloads init for the new variant:
  - `downloading init segment variant=1 url=http://127.0.0.1:61234/init/v1_init.bin`
  - `init segment downloaded variant=1 total=44 committed_len=44`
- The failure happens immediately after that unexpected init insertion.

## Entry 012
Time: 2026-03-13 Europe/Moscow

Goal of this slice:
- Remove duplicate / external writes to `Timeline.byte_position`.
- Keep reader-cursor ownership inside the `Read + Seek` path that Symphonia drives.

What was changed:
- Removed external cursor priming before anchor seek in `crates/kithara-audio/src/pipeline/source.rs`.
- Changed HLS `commit_seek_landing(...)` in `crates/kithara-hls/src/source.rs` to use the decoder-landed `Timeline.byte_position()` as the authoritative byte offset.
- Removed `Timeline.byte_position` mutation from HLS `read_at(...)`.
- Removed `Timeline.byte_position` mutation from file `read_at(...)` in `crates/kithara-file/src/session.rs`.

Tests added or updated:
- `tests/tests/kithara_audio/stream_source_tests.rs`
  - `seek_anchor_does_not_move_stream_before_exact_decoder_seek_from_eof`
- `tests/tests/kithara_hls/source_internal_cases.rs`
  - `commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant`
- `crates/kithara-hls/src/source.rs`
  - `read_at_does_not_advance_timeline_position`
- `crates/kithara-file/src/session.rs`
  - updated `test_file_source_read_at`
  - added `file_source_read_at_does_not_advance_timeline_position`

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_audio::stream_source_tests -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant -- --nocapture`
- `cargo test -p kithara-hls source::tests -- --nocapture`
- `cargo test -p kithara-file session::tests -- --nocapture`

Result:
- All four targeted suites passed.
- The “move the timeline externally to help seek” idea is now explicitly rejected by tests.

Important new observation from the heavy DRM repro:
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm -- --nocapture`
  still fails.
- After this refactor the failure shape is cleaner:
  - true EOF is reached at `range_start=25337253 total_bytes=25337253`
  - later `commit_seek_landing: could not resolve landed segment seek_epoch=1 variant=3 landed_offset=25337253`
  - then a later random seek fails with `seek anchor path: exact decoder seek failed err=SeekFailed("unexpected end of file")`

Interpretation:
- External `Timeline` cursor writes were not the root fix; removing them was still correct.
- The remaining bug is now more clearly about reusing an exhausted exact-seek session after true EOF, not about anchor priming or duplicate cursor commits.

## Entry 013
Time: 2026-03-13 Europe/Moscow

Follow-up after Entry 012:
- `kithara-file` still had one non-Stream writer of reader position:
  `Progress::set_read_pos()` was aliasing downloader demand into `Timeline.byte_position`.

What was changed:
- Split file reader demand from reader cursor in `crates/kithara-file/src/session.rs`.
- `Progress::read_pos()` / `set_read_pos()` now use their own atomic field and only wake the downloader.
- `Timeline.byte_position` is no longer written from file `wait_range()` / file demand coordination.

What was tested:
- `cargo test -p kithara-file session::tests -- --nocapture`

Result:
- File session tests passed.

Current production callsites of `Timeline.set_byte_position(...)` after this slice:
- `crates/kithara-stream/src/stream.rs` inside `Read`/`Seek`
- test code only

Interpretation:
- The reader cursor ownership is now narrowed to the stream layer, which is the boundary Symphonia actually drives.

## Entry 012
Time: 2026-03-13 Europe/Moscow

What was changed:
- Added a stale-read epoch guard in `crates/kithara-stream/src/stream.rs` so `Stream::read()` aborts the read with `io::Error::other("seek pending")` if `seek_epoch` changes after `wait_range()` or after `read_at(...)`.
- Added a source-level stale-read guard in `crates/kithara-hls/src/source.rs` so `read_at()` returns `ReadOutcome::Retry` instead of committing a stale `byte_position` when `seek_epoch` changes during the read.
- Changed `crates/kithara-hls/src/source.rs` so `commit_seek_landing(Some(anchor))` uses the resolved HLS anchor as the authoritative source-level position instead of `timeline.byte_position()`.
- Removed same-codec decoder recreation on stale base-offset drift in `crates/kithara-audio/src/pipeline/source.rs`; decoder recreation is now gated only by actual codec change.
- Added regressions in `tests/tests/kithara_hls/source_internal_cases.rs` and `tests/tests/kithara_audio/stream_source_tests.rs` covering stale decoder byte-position after seek and same-codec stale-base-offset seeks.

What was tested:
- `cargo test -p kithara-stream read_aborts_when_seek_epoch_changes_after_wait -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy commit_seek_landing_uses_anchor_when_decoder_byte_position_is_stale -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light same_codec_seek_with_stale_base_offset_does_not_recreate_decoder -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light stress_variant_only_seeks_do_not_recreate_decoder -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm -- --nocapture`

Result:
- All focused regression tests passed.
- The heavy DRM revisit repro still failed with `next_chunk timeout at stage='repro_random_seek_10_chunk_0'`.

Key observations:
- A decisive trace showed that `timeline.byte_position()` is not a reliable "actual landed offset" after `Symphonia` time seek on the virtual HLS stream.
- Example from the trace:
  - seek target near `18s` resolved anchor `segment_index=3 byte_offset=2045360`
  - after successful decoder seek, `commit_seek_landing(...)` observed `timeline.byte_position()=20258041` and wrongly committed `segment_index=29`
- This means the previous landed-offset reconciliation was misusing buffered reader state as if it were the decoder's logical landing point.
- Removing the `stale_base_offset` recreate path also exposed that same-codec `Flac -> Flac` seeks had been incorrectly entering `RecreatingDecoder` under the synthetic `CodecChange` cause.

Updated interpretation:
- The virtual-stream/decoder contract was being misused in two different ways:
  1. source-side seek landing was derived from stale buffered byte position
  2. same-codec offset drift was treated as if it required decoder recreation
- Both were real regressions and were fixed.
- The remaining heavy timeout is now further downstream, likely in HLS/downloader reconciliation after repeated same-codec revisit seeks rather than in decoder recreation policy itself.

## Entry 012
Time: 2026-03-13 Europe/Moscow

What was changed:
- Fixed `handle_tail_state()` in `crates/kithara-hls/src/downloader.rs` so tail-gap rewind only happens after a real midstream switch.
- Added a regression proving tail handling must not rewind into a never-committed variant when `had_midstream_switch == false`.

What was tested:
- `cargo test -p kithara-hls tail_state_does_not_rewind_unseen_variant_without_midstream_switch -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_decode::hls_abr_variant_switch::test_abr_variant_switch_no_byte_glitches -- --nocapture`

Result:
- The new unit regression passed.
- The heavy ABR byte-glitch integration repro passed.

Key observation:
- One remaining ABR regression was not in decoder/FSM at all.
- Tail handling mixed future ABR preference with committed runtime layout and could rewind into a variant that had never been entered.

Interpretation:
- This removed one class of same-codec ABR corruption without adding any new fallback or decoder recreation.

## Entry 013
Time: 2026-03-13 Europe/Moscow

What was changed:
- Relaxed `request_on_demand_segment()` metadata-miss handling in `crates/kithara-hls/src/source.rs`:
  - temporary `variant_out_of_range` now emits `SeekMetadataMiss` and keeps waiting for a bounded number of spins
  - it only fails with `SegmentNotFound` after the miss threshold is exceeded
- Lowered `WAIT_RANGE_MAX_METADATA_MISS_SPINS` to keep the failure path bounded and deterministic.
- Reworked switched-layout offset inference in `crates/kithara-hls/src/downloader.rs`:
  - replaced the constant-delta assumption with an anchor-based model
  - the first switched segment becomes the layout anchor
  - later segments use post-anchor delta that includes the inserted init length
- Added downloader regressions covering:
  - shifted-layout inference after the switch flag clears
  - no inference from a single shifted segment
  - shifted-layout resolution after the switch flag clears

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::test_wait_range_returns_ready_when_data_pushed -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::test_wait_range_transient_eof_with_zero_total_waits_for_data -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::test_wait_range_missing_metadata_fails_fast_with_diagnostic -- --nocapture`
- `cargo test -p kithara-hls loaded_segment_offset_mismatch_ -- --nocapture`
- `cargo test -p kithara-hls resolve_byte_offset_uses_inferred_shifted_layout_after_flag_clears -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_abr::stress_seek_during_abr_switch_real_decoder_hls -- --nocapture`

Result:
- All source-level and downloader-level regressions passed.
- `stress_seek_during_abr_switch_real_decoder_hls` passed.
- After these fixes, the remaining heavy failures split into two different clusters:
  1. DRM/cross-codec revisit after true EOF on ephemeral storage.
  2. `stress_random_seek_read_hls_encrypted_medium`, which still trips the 1s downloader `HangDetector`.

Key observation:
- The constant-delta model for switched layouts was wrong for cross-codec switches at a non-zero segment.
- When the switch inserts init before the first switched media segment, the anchor segment and all later segments no longer share the same delta.

Interpretation:
- Stale-offset handling is now materially better for switched layouts.
- The remaining failures are no longer one bug; they are separate lifecycle problems and must be investigated independently.

## Entry 014
Time: 2026-03-13 Europe/Moscow

What was changed:
- Fixed `resolve_byte_offset()` in `crates/kithara-hls/src/downloader.rs` so a shifted layout no longer places revisit re-downloads at the current EOF watermark.
- Added a new unit regression proving that re-downloading an earlier segment after full shifted-layout playback must reuse that segment's inferred shifted offset, not append at the end.
- Kept the existing shifted-layout test but updated it to cover the next unseen shifted segment instead of a revisit.
- Added a guard in `commit()` dropping stale cross-codec fetches from the pre-switch layout once a switched anchor is already committed.
- Reworked `crates/kithara-stream/src/backend.rs` batch handling:
  - replaced `join_all` with incremental batch completion tracking
  - preserved in-order commit of the ready prefix
  - added a backend regression proving the first ready batch item commits before the slowest fetch completes

What was tested:
- `cargo test -p kithara-hls resolve_byte_offset_ -- --nocapture`
- `cargo test -p kithara-hls loaded_segment_offset_mismatch_ -- --nocapture`
- `cargo test -p kithara-hls --lib -- --nocapture`
- `cargo test -p kithara-stream batch_commits_ready_prefix_without_waiting_for_slowest_fetch -- --nocapture`
- `cargo test -p kithara-stream demand_must_not_wait_for_step -- --nocapture`
- `cargo test -p kithara-stream --lib -- --nocapture`

Result:
- `kithara-hls --lib` passed (`88 passed`).
- `kithara-stream --lib` passed (`37 passed`).
- The new revisit regression initially failed exactly as expected:
  - actual offset `1340`
  - expected shifted revisit offset `1120`
- After the fix, the regression passed.

Key observations:
- The remaining DRM revisit problem was not just EOF state. A deeper placement bug remained:
  - once a cross-codec shifted layout existed, `resolve_byte_offset()` used `download_position` for every later commit in that variant
  - after full playback this made revisit re-downloads append at EOF instead of returning to their real shifted segment offset
- The encrypted-medium hang looks structurally consistent with batch progress being hidden by `join_all`:
  - one slow fetch could block visibility of earlier ready completions
  - `run_downloader` therefore saw no observable progress and tripped the 1s hang detector

Environment note:
- Native live/integration tests that require the local HTTP fixture are currently blocked in this environment.
- Both the test fixture and a standalone `python` bind to `127.0.0.1:0` fail with `Operation not permitted (os error 1)`.
- This means live DRM-heavy verification could not be rerun after the latest fixes from this shell session, even though unit-level regressions for the root causes are now green.

## Entry 012
Time: 2026-03-13 Europe/Moscow

What was changed next:
- Fixed `handle_tail_state()` in `crates/kithara-hls/src/downloader.rs` so tail-gap rewind into another variant is allowed only after a real midstream switch.
- Adjusted `wait_range()` metadata-miss behavior in `crates/kithara-hls/src/source.rs` so temporary metadata absence still waits briefly for pushed data, but repeated misses still fail with a diagnostic.
- Replaced the old constant-delta switched-layout inference in `crates/kithara-hls/src/downloader.rs` with an anchor-based model that distinguishes:
  - the first switched segment
  - the post-anchor shifted segments

New tests:
- Added a downloader regression proving tail-state must not rewind into an unseen variant without a midstream switch.
- Added downloader regressions proving shifted-layout inference must survive after the one-shot switch flag clears, but must not be inferred from a single shifted segment.

What was retested:
- `cargo test -p kithara-hls tail_state_does_not_rewind_unseen_variant_without_midstream_switch -- --nocapture`
- `cargo test -p kithara-hls loaded_segment_offset_mismatch_ -- --nocapture`
- `cargo test -p kithara-hls resolve_byte_offset_uses_inferred_shifted_layout_after_flag_clears -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_decode::hls_abr_variant_switch::test_abr_variant_switch_no_byte_glitches -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::test_wait_range_returns_ready_when_data_pushed -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::test_wait_range_transient_eof_with_zero_total_waits_for_data -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::source_internal_cases::test_wait_range_missing_metadata_fails_fast_with_diagnostic -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_abr::stress_seek_during_abr_switch_real_decoder_hls -- --nocapture`

Result:
- All targeted unit tests above passed.
- The HLS real-decoder ABR stress test passed.
- The remaining failures split into two different classes:
  1. DRM revisit / replay after true EOF on switched layouts.
  2. Encrypted-medium downloader hang detection during startup / batch fetch.

Key observations:
- The stale-offset regressions were not caused by decoder recreation policy. They came from conflating metadata offsets with shifted switched-layout offsets after init insertion.
- The remaining DRM logs now show a cleaner sequence:
  - successful cross-codec switch and decoder recreation
  - full playback to true EOF
  - later revisit with evicted resources triggering re-download pressure
- The remaining encrypted-medium failure no longer looks like stale-offset math or ABR layout inference. It looks like downloader progress accounting or a blocking batch path.

## Entry 012
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Focused on the smallest remaining integration repro:
  - `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_decode::hls_abr_variant_switch::test_abr_variant_switch_no_byte_glitches -- --nocapture`

What the log showed:
- The failure no longer involved init insertion.
- Instead, after `ABR variant switch from=0 to=2 cross_codec=false reason=UpSwitch`, the downloader logged:

## Entry 015
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Moved to the smaller `file/mp3` regression cluster before another full-suite run.
- Added stage-specific preload timeout diagnostics in `tests/tests/kithara_play/resource_regressions.rs` so hangs report the exact stage instead of a generic 10s timeout.
- Added a focused preload regression in `tests/tests/kithara_audio/audio_tests.rs`:
  - `test_audio_preload_rearms_after_seek`

What the focused repro proved:
- The failure was not in `kithara-file` demand fetching itself.
- The first bad state happened immediately after the first seek in the first mp3 session.
- Logs showed:
  - seek was applied successfully in `StreamAudioSource`
  - post-seek file reads happened
  - then `Resource::preload()` waited forever for a new preload notification

Root cause found:
- `Audio::seek()` resets consumer-side preload state:
  - clears buffered PCM
  - sets `preloaded = false`
  - moves the consumer into a new post-seek wait
- `audio_worker::TrackSlot` did not reset its own preload state on `seek_epoch` change.
- As a result, after seek the consumer waited for a fresh preload notification, while the worker still believed preload had already completed for that track.

What was changed:
- Added `seek_epoch` tracking to `crates/kithara-audio/src/pipeline/audio_worker.rs`.
- Added `sync_seek_epoch()` in `TrackSlot` and call it at the start of `step()`.
- On epoch change the worker now resets:
  - `pending_fetch`
  - `chunks_sent`
  - `preloaded`
  - `eof_sent`
- Added worker regression:
  - `worker_preload_notify_rearms_after_seek`

What was tested:
- `cargo test -p kithara-audio worker_preload_notify_rearms_after_seek -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_audio::audio_tests::test_audio_preload_rearms_after_seek -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::player_resource_mp3_reopen_same_cache_keeps_backward_seek_ephemeral -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::player_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::player_worker_hls_then_unavailable_mp3_then_mp3_recovery_ephemeral -- --nocapture`

Result:
- The worker regression passed.
- The audio-level preload regression passed.
- `mp3 reopen same cache keeps backward seek` passed.
- `HLS -> mp3 reopen keeps backward seek` passed.
- `HLS -> unavailable mp3 -> mp3 recovery` still failed, but now as the remaining narrower player lifecycle bug.

Interpretation:
- The previously observed “mp3 first transition broken, second transition better” behavior had a real protocol bug behind it.
- The bug was not speculative cache folklore; it was a concrete consumer/worker preload-state split across seek epochs.
- The current remaining file/player failure now excludes the simple post-seek preload path and is narrower than before.
  - `playlist tail reached with gaps; rewinding to first missing segment variant=2 segment_index=0 num_segments=3`
- No segment fetch followed immediately; the downloader entered an unnecessary backfill path for the unseen target variant.

Root cause found:
- `handle_tail_state()` was allowed to rewind into the new ABR variant even when no real midstream switch had happened yet.
- In this state:
  - `had_midstream_switch == false`
  - committed data still belonged to the old variant
  - playback was not at EOF yet
- Rewinding to `segment 0` of the unseen variant was wrong. It mixed "current committed layout" with "future ABR preference" and pulled the downloader into an unnecessary path.

What was changed:
- Added a failing unit regression in `crates/kithara-hls/src/downloader.rs`:
  - `tail_state_does_not_rewind_unseen_variant_without_midstream_switch`
- Changed `handle_tail_state()` so tail-gap rewind is only allowed when `had_midstream_switch` is already true.

What was tested:
- `cargo test -p kithara-hls tail_state_does_not_rewind_unseen_variant_without_midstream_switch -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_decode::hls_abr_variant_switch::test_abr_variant_switch_no_byte_glitches -- --nocapture`

Result:
- The new unit regression passed after the fix.
- The integration repro passed.

Interpretation:
- This confirms another architectural smell in the current HLS runtime:
  - tail backfill logic was acting on an ABR preference before the byte layout had actually switched.
- The correct invariant is stricter:
  - backfill rewind is valid only after an actual committed midstream switch, not merely after an ABR decision.

Updated hypothesis:
- The remaining root cause is no longer decoder fallback/recovery.
- The remaining root cause is an HLS downloader/runtime path that still injects init on a same-codec variant switch even though the unit-level init policy tests say it should not.
- The next step is to trace the exact runtime init decision path (`build_demand_plan`, `build_batch_plans`, `switch_needs_init`, and any segment fetch path that can bypass them).
- Extended `DecodeError::is_interrupted()` so `Backend(Box<io::Error>)` carrying `seek pending` is recognized through the error chain.

What was tested:
- `cargo test -p kithara-hls build_demand_plan_same_codec_variant_change_does_not_request_init -- --nocapture`
- `cargo test -p kithara-hls build_batch_plans_same_codec_variant_change_keeps_metadata_layout -- --nocapture`
- `cargo test -p kithara-decode test_backend_seek_pending_counts_as_interrupted -- --nocapture`
- `cargo test -p kithara-decode test_backend_other_io_is_not_interrupted -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`

Result:
- The `+44` layout drift is gone from the heavy stress logs.
- `segment in plan window has stale offset` count dropped to `0`.
- `demand segment has stale offset` count dropped to `0`.
- `seek pending` no longer appears as a decode error and `HangDetector` no longer triggers in the heavy stress test.
- The heavy stress test still fails, but now as a plain `10s timeout` with no hang signal.

Key observations from the new runs:
- The earlier same-codec ABR switch bug was real: the system was inserting a new 44-byte WAV init header into the middle of the virtual stream, then later treating its own shifted layout as stale.
- After the init-policy fix, the same segments now commit at metadata offsets (`...0044`) instead of shifted offsets (`...0088`).
- After the decode-error fix, the remaining failure is no longer about bad seek interruption handling.

## Entry 011
Time: 2026-03-13 Europe/Moscow

What was tested:
- Ran the heavy stress test again with `RUST_LOG=info` to remove debug-tracing as a candidate root cause:
  - `env RUST_LOG=info cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`
- Ran the same stress test with fetch tracing focused on media cache/network behavior:
  - `env RUST_LOG='info,kithara_hls::fetch=trace' cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`

Result:
- The timeout is not caused by debug logs alone.
- With `RUST_LOG=info`, the test still timed out after 10 seconds and reached only `iteration=1000`.
- The phase-2 progress still reported:
  - `dead_seeks=0`
  - `total_retries=0`
  - `max_retries_single=0`

## Entry 015
Time: 2026-03-13 Europe/Moscow

What architectural step was taken:
- Added `AssetStore::acquire_resource(...)` and `AssetStore::acquire_resource_with_ctx(...)` as an explicit mutable-access entry point in `crates/kithara-assets/src/unified.rs`.
- Added access provenance to the outer `LeaseResource` wrapper in `crates/kithara-assets/src/lease.rs`.
- `write_at`, `commit`, `fail`, and `reactivate` now panic if the resource was obtained via `open_resource(...)` / `open_resource_with_ctx(...)`.
- Kept `open_resource*` as the read-only entry point.

What runtime paths were migrated immediately:
- HLS segment/key/playlist write paths in `crates/kithara-hls/src/fetch.rs` now use `acquire_resource*`.
- Remote file download session creation in `crates/kithara-file/src/session.rs` now uses `acquire_resource(...)`.

Why this was done:
- The current architecture still allows mutable resource use to enter through the wrong API.
- Instead of adding more logs or another heuristic, the protocol now fails fast at the resource boundary and points directly at remaining write-via-open call sites.

What was tested:
- `cargo check -p kithara-assets`
- `cargo check -p kithara-hls`
- `cargo check -p kithara-file`
- `cargo test -p kithara-assets open_resource_panics_on_write -- --nocapture`
- `cargo test -p kithara-assets builder_defaults_all_enabled -- --nocapture`
- `cargo test -p kithara-assets -- --nocapture`
- `cargo test -p kithara-hls --lib -- --nocapture`
- `cargo test -p kithara-file -- --nocapture`

Result:
- All three `cargo check` runs passed.
- The new contract test passed:
  - `open_resource_panics_on_write`
- Updated local builder/unit tests using `acquire_resource(...)` passed.
- Broad package tests now fail exactly at remaining write-via-open sites.

Confirmed remaining migration sites from test failures:
- `kithara-assets`:
  - `tests/resource_state.rs`
- `kithara-hls`:
  - downloader tests seeding ephemeral resources via `open_resource(...)`
  - fetch tests evicting/reseeding resources via `open_resource(...)`
  - source tests preloading media/init resources via `open_resource(...)`
- `kithara-file`:
  - session/downloader unit tests that seed the backing resource via `open_resource(...)`

Key interpretation:
- This fast-fail confirms the intended boundary is useful:
  - `open_resource*` is a read-only/view API
  - `acquire_resource*` is the mutable ownership API
- It also exposes a deeper architectural point:
  - adding a new status alone would not have helped
  - the protocol needed a separate mutable-acquisition path, and the outer resource wrapper was the right place to enforce it

Next architectural implication:
- HLS-local dedupe / request lifecycle can now be migrated with a clearer separation:
  - assets layer owns mutable resource admission
  - HLS owns segment/layout workflow
  - generic backend owns scheduling only
  - `integrity_errors=0`
- Fetch tracing showed that media/network is no longer the dominant cost in phase 2:
  - `start_fetch: starting network fetch` count was `49`
  - `start_fetch: cache hit` count was `27`
  - by early phase 2, variant `v1` segments were already broadly loaded/cached

Key observations:
- The remaining 10-second timeout persists even after most media segments are already local.
- This rules out the earlier queue/hang issue, and strongly weakens the hypothesis that logging or repeated network fetches are the primary bottleneck.
- `total_retries=0` means the outer `read_with_retry()` loop is not sleeping, but it does **not** prove that `audio.read()` is cheap internally.
- The current HLS anchor seek path still performs:
  1. source/layout preparation from the anchor
  2. `decoder.seek(anchor.segment_start)`
  3. internal decode-and-drop of `position - anchor.segment_start` via seek-skip before returning the first chunk

Important code observation:
- There is already an audio unit test in `tests/tests/kithara_audio/stream_source_tests.rs` that explicitly locks in this behavior:
  - it expects `decoder.seek()` to receive `anchor.segment_start`
  - it expects the first fetched PCM chunk to be trimmed by the relative in-segment offset

Updated hypothesis:
- The current bottleneck is most likely CPU/control-flow inside the anchored seek path, not downloader I/O.
- More specifically, the `decoder.seek(anchor.segment_start) + internal skip` contract is now the strongest candidate.
- If the virtual `Read + Seek` stream is now correct enough after landed-offset reconciliation, the next minimal experiment should be:
  - keep anchor resolution only for HLS/source layout preparation
  - call `decoder.seek(position)` exactly
  - stop using post-seek decode-and-trim as the normal same-codec path
- The log is now dominated by useful work and stale-fetch drops, not by layout-repair churn or seek-pending decode failures.

Updated hypothesis:
- The current root cause is now primarily throughput: under 2000 rapid seeks, the downloader still does too much stale speculative work before each new seek epoch invalidates it.
- The next investigation should focus on seek-era prefetch policy and whether sequential batch work should be suppressed or narrowed until the post-seek read path has resumed.

## Entry 012
Time: 2026-03-13 Europe/Moscow

What was tested:
- Ran the new focused unit regression in `crates/kithara-audio/src/pipeline/audio.rs`:
  - `cargo test -p kithara-audio read_preserves_partial_chunk_tail_across_calls -- --nocapture`

Result:
- The test failed exactly as expected and confirmed the new hypothesis.

Key observations:
- The second `read()` call started from the next chunk (`4608.0...`) instead of first draining the tail of the previous chunk.
- This proves `Audio::read()` is currently allowed to overwrite `self.current_chunk` even when the output buffer is already full and the old chunk still has unread samples.
- The failure matches the heavy integration symptom precisely:
  - decoder chunk size: `2304` stereo frames
  - output read size in the stress test: `2205` stereo frames
  - dropped tail per boundary: `2304 - 2205 = 99` frames
- The phase-3 heavy failure from the previous run was:
  - `frame_diff = 85932`
  - `continuity_breaks = 868`
  - `85932 / 868 = 99`

Updated hypothesis:
- The current remaining functional failure is not in seek landing anymore.
- The dominant remaining bug is consumer-side PCM loss in `Audio::read()`.
- The minimal fix should be to stop calling `fill_buffer()` once the caller's output buffer is already full, so partially-consumed chunks remain resident until the next `read()` call.

## Entry 013
Time: 2026-03-13 Europe/Moscow

What was changed:
- Fixed `Audio::read()` in `crates/kithara-audio/src/pipeline/audio.rs` so it no longer calls `fill_buffer()` after the caller's output buffer is already full.

What was tested:
- `cargo test -p kithara-audio read_preserves_partial_chunk_tail_across_calls -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light seek_anchor_ -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light stress_variant_only_seeks_do_not_recreate_decoder -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_mmap -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy`

Result:
- The focused unit regression passed after the fix.
- The seek-anchor and same-codec seek light tests passed.
- `stress_seek_lifecycle_with_zero_reset_mmap` passed in `5.22s`.
- Full integration suites are still red, but with a different failure profile than before.

New passing signal:
- The old frame-loss symptom is gone from the original heavy lifecycle test.
- The exact seek + `Audio::read()` fix combination eliminated both:
  - the old `10s timeout`
  - the old `99 frames per boundary` loss in `stress_seek_lifecycle_with_zero_reset_mmap`

Remaining failures from `suite_light`:
- `kithara_audio::audio_tests::test_seek_complete_emitted_only_after_output_commit`
- `kithara_audio::audio_tests::test_seek_emits_matching_playback_progress`
- `kithara_audio::stream_source_tests::abr_switch_must_not_lose_samples`
- `kithara_audio::stream_source_tests::seek_during_pending_format_change_retries_on_new_decoder`

Remaining failures from `suite_heavy`:
- Multiple DRM/live/HLS stress scenarios now hang in `kithara_audio::pipeline::source::decode_next_chunk`
- `kithara_audio::pipeline::audio::recv_outcome_blocking` also trips the hang detector downstream in some ABR stress scenarios
- `kithara_hls::source_internal_cases::format_change_segment_range_prefers_loaded_init_bearing_segment`
- `kithara_hls::source_internal_cases::test_wait_range_missing_metadata_fails_fast_with_diagnostic`
- `kithara_hls::stress_chunk_integrity::{ephemeral,mmap}`
- `kithara_hls::stress_seek_abr::*` (several DRM/HLS variants)
- `kithara_hls::stress_seek_abr_audio::stress_seek_abr_audio`
- `kithara_hls::stress_seek_random::stress_random_seek_read_hls_encrypted_medium`

Key observations:
- The remaining failures cluster into two groups.
- Group 1: consumer/event contract regressions.
  - `test_seek_complete_emitted_only_after_output_commit` now reports that `read()` did not commit PCM output in the expected place.
  - `test_seek_emits_matching_playback_progress` now gets `None` where it expected a seek epoch in playback progress.
- Group 2: codec-changing ABR / pending format-change regressions.
  - `seek_during_pending_format_change_retries_on_new_decoder` now shows that the pending format change was not applied during seek recovery (`0` vs expected `1000`).
  - Real DRM and HLS stress tests hang inside `decode_next_chunk` immediately after cross-codec variant switches.
  - One heavy HLS internal test also says the loaded init-bearing segment is no longer preferred for format-change range calculation.
  - Another heavy audio test reports a direction error immediately after ABR switch, which is consistent with a broken format-boundary application path rather than generic read-side PCM loss.

Updated hypothesis:
- The `Audio::read()` tail-loss bug was real and is fixed.
- The remaining architecture problem is now concentrated in the format-change / decoder-recreate path, especially when a seek intersects a pending codec-changing ABR boundary.
- The next step should focus on the failing light tests first, because they are likely the minimal repros for the heavy DRM/live hangs.

## Entry 015
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Returned to the remaining HLS revisit timeout with focus on `crates/kithara-hls/src/source.rs` and on-demand request lifecycle.
- Tried to rerun `kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm` with fresh tracing.

Environment result:
- Fresh live integration repro could not be rerun in the current environment because localhost bind is currently denied:
  - `bind test HTTP listener after retries: Operation not permitted (os error 1)`
  - a direct Python `127.0.0.1:0` bind probe also failed with `PermissionError: [Errno 1] Operation not permitted`
- Because of that, analysis continued from the previously captured `/tmp/live_ephemeral_revisit_trace.log` plus static code inspection.

Key observation from the saved trace:
- The stall pattern near `repro_random_seek_10_chunk_0` was:
  - `wait_range: metadata on-demand segment load variant=3 segment_index=33`
  - downloader logs `demand segment re-download variant=3 segment_index=33`
  - then `wait_range` stays in `WaitingDemand` until timeout
- This pointed to request-lifecycle desynchronization rather than to decoder recreation.

Root cause found:
- `wait_range()` kept its own local `on_demand_epoch`, while the real request lifecycle lived in `SharedSegments::pending_segment_request`.
- `request_on_demand_segment()` returned `true` whenever metadata resolved, even if `enqueue_segment_request()` deduped the request and did not actually queue it.
- After that, `wait_range()` believed demand was "already pending" and stopped re-issuing requests for the same `seek_epoch`, even after downloader cleared the pending slot.
- This is exactly the kind of shadow-state bug created by the dedupe compensation layer.

What was changed:
- In `crates/kithara-hls/src/source.rs`:
  - `push_segment_request(...)` now returns the actual enqueue result.
  - `request_on_demand_segment(...)` now returns whether a request was really queued.
  - `wait_range()` no longer keeps local `on_demand_epoch`; it now derives `WaitingDemand` directly from `SharedSegments::pending_segment_request` via `has_pending_segment_request(...)`.
- Added two unit regressions:
  - `on_demand_request_returns_false_when_dedupe_suppresses_enqueue`
  - `wait_range_reissues_request_after_pending_request_is_cleared`

What was tested:
- `cargo test -p kithara-hls on_demand_request_returns_false_when_dedupe_suppresses_enqueue -- --nocapture`
- `cargo test -p kithara-hls wait_range_reissues_request_after_pending_request_is_cleared -- --nocapture`
- `cargo test -p kithara-hls --lib -- --nocapture`

Result:
- Both new regressions passed after the fix.
- Full `kithara-hls` unit tests passed: `91 passed; 0 failed`.

Updated interpretation:
- The dedupe mechanism itself was not the whole bug; the worse regression was duplicating its state inside `wait_range()`.
- This was a concrete example of why the current pending/request layering is fragile: the source and downloader were answering "is there an active demand request?" from different sources of truth.
- Live integration repro still needs rerun once bind works again, but the local source-level stall mechanism is now covered and fixed.

## Entry 016
Time: 2026-03-13 Europe/Moscow

What was changed:
- Removed the ephemeral-demand fallback in `crates/kithara-hls/src/downloader.rs` that forced `segment_loaded_for_demand(...)` to return `false` even when segment resources were still present.
- Updated the HLS demand regression so ephemeral mode now reuses present resources and only requests re-download after actual eviction.
- Added exact-EOF source regressions in `crates/kithara-hls/src/source.rs`:
  - `set_seek_epoch_keeps_exact_eof_visible_until_seek_lands`
  - `wait_range_uses_known_total_bytes_for_exact_eof`
- Changed `crates/kithara-hls/src/source.rs` so:
  - `set_seek_epoch()` no longer clears `timeline.eof()`
  - `is_past_eof()` treats `range.start >= total_bytes` as EOF whenever `total_bytes` is known, even if the separate EOF flag has not yet been refreshed

What was tested:
- `cargo test -p kithara-hls segment_loaded_for_demand_tracks_resource_presence_in_ephemeral_mode -- --nocapture`
- `cargo test -p kithara-hls set_seek_epoch_ -- --nocapture`
- `cargo test -p kithara-hls exact_eof -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm -- --nocapture`

Result:
- All new/focused `kithara-hls` unit regressions passed.
- The old log spam `ephemeral: force re-download to refresh LRU position` is gone.
- The heavy DRM revisit repro still fails at:
  - `next_chunk timeout at stage='repro_random_seek_10_chunk_0' (is_eof=false)`

Key observations from the new heavy runs:
- The earlier exact-EOF ambiguity was real:
  - the source now logs `wait_range: EOF range_start=25337253 total_bytes=25337253`
  - the decoder also logs `decode complete (true EOF)`
- But this was not the only remaining blocker. The live DRM repro still times out later in the same scenario.
- The dominant log sequence right before the timeout is now:
  - `segment metadata present but resources evicted, need re-download`
  - `segment in plan window lost resources, forcing refresh`
  - `batch fetch error e=Writer(SinkWrite(Failed("cannot write to committed resource")))`
- This shifts the remaining investigation away from the previous demand fallback and toward the refresh/re-download path for evicted resources on ephemeral storage.

Updated interpretation:
- Removing the forced-demand fallback was correct and simplified the HLS demand contract.
- Preserving exact EOF from `total_bytes` was also correct and removed one optimistic-state bug in the seek/reset window.
- The remaining live DRM revisit failure is now concentrated in the downloader refresh path after eviction, not in the removed fallback itself.

## Entry 017
Time: 2026-03-13 Europe/Moscow

What was changed:
- Added a new public assets-layer contract in `crates/kithara-assets`:
  - `AssetResourceState = Missing | Active | Committed { final_len } | Failed`
  - `Assets::resource_state(&ResourceKey)` as a side-effect-free query
  - `AssetStore::resource_state(...)` as the public entrypoint
- Implemented backend-side state inspection:
  - `MemAssetStore` reports `Missing` without creating a resource
  - `DiskAssetStore` inspects the mapped path via filesystem metadata instead of opening it
- Made `CachedAssets` the single live-state resolver:
  - if a matching handle is already live in cache, return its runtime status
  - otherwise delegate to the backend
- Rewired `has_resource()` to derive from `resource_state()` instead of its own cache-only heuristic.
- Added integration coverage in `crates/kithara-assets/tests/resource_state.rs` for:
  - disk state transitions across multiple files
  - ephemeral memory state transitions, failure, remove, and LRU eviction
  - disk processing, pins, and asset eviction across multiple asset roots

What was tested:
- `cargo test -p kithara-assets --test resource_state -- --nocapture`
- `cargo test -p kithara-assets`

Result:
- All `kithara-assets` tests passed:
  - unit tests: `91 passed`
  - integration `resource_state`: `3 passed`
- This gives the assets layer one canonical runtime answer for "what is the state of this resource right now?" instead of reconstructing it from `has_resource()` and later `open_resource().status()`.

Key observations:
- The old `has_resource()` contract was too weak: it only answered "is there a committed handle in this cache right now?" and could not express `Active`, `Failed`, or backend-persisted committed state after cache eviction.
- `resource_state()` is intentionally side-effect-free, which matters because `open_resource()` on a missing resource creates a new `Active` resource and therefore cannot be used as an inspector.
- Full-store pin lifetime is broader than plain `LeaseAssets` unit tests suggest:
  - cached `LeaseResource` entries keep the lease guard alive
  - so dropping the user handle is not enough to unpin while the store cache still holds that resource
  - dropping the owning cached store releases that pin

Updated interpretation:
- For resource residency/state, the single source of truth now lives in `kithara-assets`, not in HLS heuristics.
- There is still a separate architectural smell around lease lifetime being coupled to cache residency; that was exposed by the new integration tests but not changed in this slice.

## Entry 018
Time: 2026-03-13 Europe/Moscow

What was changed:
- Fixed the lease/cache ownership bug in `crates/kithara-assets/src/store.rs` by reordering decorators:
  - before: `... -> LeaseAssets -> CachedAssets`
  - after: `... -> CachedAssets -> LeaseAssets`
- This means:
  - cache now stores only processed resource handles
  - pin state now belongs only to the outward-facing lease wrapper returned to the caller
  - cache residency no longer extends pin lifetime
- Added a direct regression in `crates/kithara-assets/tests/resource_state.rs`:
  - dropping the last user handle must eagerly unpin even while the `AssetStore` itself remains alive
- Added `Debug` for `LeaseAssets` because it is now the outer store type inside `AssetStore`.

What was tested:
- `cargo test -p kithara-assets --test resource_state disk_resource_state_tracks_processing_pins_and_asset_eviction -- --nocapture`
- `cargo fmt --all`
- `cargo test -p kithara-assets`

Result:
- The new pin-lifetime regression passed after the decorator reorder.
- Full `kithara-assets` verification stayed green.

Key observations:
- This confirms the right fix was ownership, not more state:
  - no extra flags
  - no cache cleanup hooks
  - no "force unpin on eviction" special case
- The previous behavior was an architectural mistake:
  - `CachedAssets` was caching `LeaseResource`
  - `LeaseResource` carried the live `LeaseGuard`
  - therefore cache lifetime accidentally became part of pin lifetime
- After the reorder, the pin contract matches the public semantics again:
  - `open_resource()` pins
  - dropping the returned handle unpins
  - cache only controls resource reuse, not lease ownership

Updated interpretation:
- `kithara-assets` now has a cleaner separation of concerns on both axes:
  - `resource_state()` is the single source of truth for resource residency/state
  - lease lifetime is again controlled by user-visible handles instead of hidden cache entries

## Entry 019
Time: 2026-03-13 Europe/Moscow

What was investigated:
- The downloader architecture was checked against the special case `file ~= HLS with one segment`.
- Two new downloader regressions were added in `crates/kithara-hls/src/downloader.rs`:
  - `single_segment_playlist_plans_only_segment_zero`
  - `single_segment_playlist_reaches_tail_after_commit`

What these tests exposed:
- `HlsDownloader::plan()` and `poll_demand()` were still taking `num_segments` from the loader/fetch path instead of from the downloader's own `PlaylistState`.
- That meant topology had two sources of truth:
  - source/downloader logic used `PlaylistState`
  - planner demand/tail logic used `Loader::num_segments()`
- In the unit setup this manifested immediately: the single-segment HLS case could not even plan `segment 0` or reach EOF after committing it.

What was changed:
- Added `HlsDownloader::num_segments(...)` as a local read from `self.playlist_state`.
- Changed:
  - `num_segments_for_demand(...)`
  - `num_segments_for_plan(...)`
  to use the downloader's own `PlaylistState` instead of `self.fetch.num_segments(...)`.
- When a variant is missing from `PlaylistState`, the downloader now reports it as `VariantNotFound` instead of silently turning topology lookup into a network-style failure.
- Also changed `FetchManager::num_segments()` to prefer its attached `playlist_state` when available, keeping the loader consistent with the same source of truth.

What was tested:
- `cargo test -p kithara-hls single_segment_playlist_plans_only_segment_zero -- --nocapture`
- `cargo test -p kithara-hls single_segment_playlist_reaches_tail_after_commit -- --nocapture`
- `cargo test -p kithara-hls downloader -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_ephemeral -- --nocapture`

Result:
- Both new `HLS(1 segment)` regressions passed after the change.
- Full HLS downloader unit suite passed: `50 passed; 0 failed`.
- The heavy seek lifecycle repro is still red.

Key observation from the heavy repro:
- The remaining failure is not the same bug as the single-segment topology split.
- The run still reaches a same-codec ABR switch:
  - `ABR variant switch from=0 to=1 cross_codec=false reason=DownSwitch`
- After that, the target variant cursor still regresses into earlier segments:
  - commits are observed for `segment_index=6`, then later `4`, `5`, `7`, `3`, `1`, `0`, `2`
- This means the current residual problem is not "planner does not know how many segments exist".
- The residual problem is a different runtime cluster: switched-layout cursor / rewind behavior is still allowing the downloader to reopen low target-variant segments after the switch.

Updated interpretation:
- Treating `PlaylistState` as the downloader's single topology source was the right architectural fix.
- It cleanly covers the `HLS(1 segment)` special case without introducing a dedicated branch.
- The heavy seek hang remains, but it is now more clearly separated from topology/source-of-truth issues.

## Entry 020
Time: 2026-03-13 Europe/Moscow

What was changed:
- Tightened the generic stream contract in `kithara-stream` so the role types are no longer free-floating declarations:
  - `StreamType::Source` is now constrained to match `Topology/Layout/Coord/Demand`
  - `Source` now exposes `topology()/layout()/coord()`
  - `Source::timeline()` now defaults to `self.coord().timeline()`
- Added generic forwarding impls in `kithara-stream` so the actual shared handles can be the canonical role types:
  - `Arc<T>: Topology`
  - `Mutex<T>` and `Arc<T>` for `LayoutIndex`
  - `Arc<T>` for `TransferCoordination`
- Rewired concrete stream types to use their real shared handles as role types instead of abstract placeholders:
  - HLS:
    - `Topology = Arc<PlaylistState>`
    - `Layout = Arc<Mutex<DownloadState>>`
    - `Coord = Arc<HlsCoord>`
  - File:
    - `Topology = ()`
    - `Layout = ()`
    - `Coord = Arc<FileCoord>`
- Updated `kithara-test-utils` memory source and the stream unit/fuzz test sources to the same contract.

Why this matters:
- Before this change, `StreamType::{Topology, Layout, Coord, Demand}` existed mostly as declarations on the marker type.
- That meant the type system still allowed structural drift:
  - the `Source` object did not have to return those same role types
  - `Timeline` could still have an independent path through manual `Source::timeline()` impls
- After this change, the generic roles are tied to the actual source object and its real shared handles.

Key observations:
- This does not make a dishonest implementation impossible, but it does remove the previous blind spot where the role types were not connected to runtime ownership at all.
- The important boundary is now explicit:
  - `Coord` owns `Timeline`
  - `Source` exposes that `Coord`
  - `StreamType` must name the same handle type that `Source` actually returns

What was tested:
- `cargo test -p kithara-stream --lib`
- `cargo test -p kithara-file --lib`
- `cargo test -p kithara-hls --lib`

Result:
- All three `lib` suites passed:
  - `kithara-stream`: `42 passed`
  - `kithara-file`: `33 passed`
  - `kithara-hls`: `111 passed`

Updated interpretation:
- This slice does not solve the heavy integration failures by itself.
- But it removes another structural blind spot:
  - the stream role types now participate in real contracts instead of documentation-only architecture.

## Entry 021
Time: 2026-03-13 Europe/Moscow

What was investigated:
- After the structural `StreamType` / `Source` refactor, the user reported that the workspace no longer built.

What was found:
- The breakage was not in prod crates already checked by the focused `lib` runs.
- The failing target was the fuzz harness:
  - `tests/fuzz/fuzz_targets/stream_read_seek.rs`
- That target still implemented the old `Source` and `StreamType` contracts and therefore missed:
  - `Topology`
  - `Layout`
  - `Coord`
  - `Demand`
  - `topology()/layout()/coord()`

What was changed:
- Reworked the fuzz `ScriptSource` exactly like the unit-test `ScriptSource`:
  - introduced a single `ScriptCoord`
  - removed direct `Timeline` ownership from the source
  - wired `DummyType` to `Topology = ()`, `Layout = ()`, `Coord = ScriptCoord`, `Demand = ()`

What was tested:
- `cargo check --workspace`

Result:
- Full workspace check is green again.

Updated interpretation:
- The structural refactor is now coherent at the workspace boundary:
  - prod crates build
  - focused `lib` suites pass
  - fuzz targets compile under the new contract
- The next remaining step is not to keep reshaping these abstractions further.
- The next remaining step is to run the full test suite and treat the red integration tests as the current primary source of truth.

## Entry 022
Time: 2026-03-13 Europe/Moscow

What changed in the observed failure surface:
- The user reported a new runtime regression outside the HLS seek cluster:
  - switching from a working HLS track to an MP3 track in `kithara-app` can crash the app
  - MP3 seek regressed asymmetrically:
    - forward seek moves only partially
    - backward seek initially does not work
    - after switching to the same MP3 again, seek works better
- The user also clarified that HLS seek itself still works in both directions.

What was verified:
- `cargo test -p kithara-file --lib -- --nocapture` is green.
- The first attempt to run a focused heavy MP3 integration repro no longer reaches runtime:
  - `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_file::live_stress_real_mp3::live_stress_real_mp3_seek_read_cache -- --nocapture`
  - fails at compile time in `tests/tests/kithara_hls/source_internal_cases.rs`

What was found:
- The integration test crate still contains old HLS helper usage:
  - imports `SharedSegments`
  - calls `make_test_source(...)` and `make_test_source_with_fetch(...)` with the pre-refactor signatures
- That means the current primary blocker for a full suite run is not the MP3 runtime bug itself, but a stale HLS test harness left behind by the structural refactor.

Current interpretation:
- There are now two independent red clusters:
  1. test-harness fallout from the HLS coordination refactor
  2. MP3/file/player lifecycle regression, likely involving first-load vs replay state
- The MP3 symptoms now point more toward file/cache/resource lifecycle than toward decoder misuse:
  - HLS seek works
  - file `lib` tests pass
  - behavior changes after a repeated switch to the same MP3

Next step:
- Restore integration-test compilation first.
- Then add regression tests for:
  - player switch to an unavailable MP3 resource must not panic
  - MP3 backward seek after prior playback / failed switch lifecycle

## Entry 023
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Whether the integration test crate still had stale stream-role implementations after the `StreamType` / `Source` refactor.

What was found:
- The initial HLS helper fallout was already repaired enough for `suite_heavy --no-run`, but three remaining integration test files still implemented the old source contract:
  - `tests/tests/kithara_audio/stream_source_tests.rs`
  - `tests/tests/kithara_stream/reader_seek_overflow.rs`
  - `tests/tests/kithara_stream/timeline_source_of_truth.rs`
- Those files still owned `Timeline` directly on the fake source and did not provide:
  - `Topology`
  - `Layout`
  - `Coord`
  - `Demand`
  - `topology()/layout()/coord()`

What was changed:
- Reworked those three test harnesses to match the new contract:
  - each fake source now owns a small local coordination object
  - each coordination object implements `TransferCoordination<()>`
  - `StreamType` now binds `Topology = ()`, `Layout = ()`, `Coord = ...`, `Demand = ()`
- In `timeline_source_of_truth`, removed direct cursor mutation from `read_at` so the test continues to validate that `Stream<T>` is the byte-position owner.

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light --no-run`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy --no-run`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light decoder_file_seek_backward -- --nocapture`

Result:
- Both integration suites compile again.
- `decoder_file_seek_backward` passes.

Updated interpretation:
- The current MP3 regression is not reproduced by the existing decode-level backward-seek test.
- The next useful tests must be higher-level and lifecycle-oriented:
  - player switch to an unavailable MP3 resource must not panic
  - file/mp3 seek after first-play vs replay / cache reuse

## Entry 024
Time: 2026-03-13 Europe/Moscow

What was investigated:
- New player-facing regression scenarios on `kithara_play::Resource`, specifically:
  - repeated transition to an unavailable remote MP3
  - reopen of the same remote MP3 with a shared cache root, then forward/backward seek again

What was changed:
- Added `tests/tests/kithara_play/resource_regressions.rs`
- Added two new integration scenarios:
  - `player_resource_repeated_unavailable_mp3_does_not_panic`
  - `player_resource_mp3_reopen_same_cache_keeps_backward_seek`
- Those tests intentionally stay above raw `Audio<Stream<File>>` and below UI/hardware:
  - they exercise the player-facing `Resource` wrapper
  - they use a deterministic local HTTP server with:
    - `/ok.mp3` supporting `HEAD` and `Range`
    - `/gone.mp3` returning `503`

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::player_resource_repeated_unavailable_mp3_does_not_panic -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::player_resource_mp3_reopen_same_cache_keeps_backward_seek -- --nocapture`

Result:
- All three new scenarios are red:
  - `player_resource_repeated_unavailable_mp3_does_not_panic`
  - `player_resource_mp3_reopen_same_cache_keeps_backward_seek_disk`
  - `player_resource_mp3_reopen_same_cache_keeps_backward_seek_ephemeral`
- Failure mode is not a plain assertion failure.
- They time out and the hang detector reports:
  - `kithara_audio::pipeline::audio_worker::run_shared_worker_loop` no progress for 1s

Updated interpretation:
- This is the first higher-level repro that aligns with the user report:
  - the issue is above decoder-only tests
  - the issue is below UI/hardware
  - the failure surface involves player-facing resource lifecycle and shared worker progress
- The next step is no longer to invent more scenarios.
- The next step is to localize where these tests stall:
  - initial preload
  - seek after preload
  - reopen after prior success
  - failed `Resource::new` followed by replay

## Entry 025
Time: 2026-03-13 Europe/Moscow

What was investigated:
- The remaining integration fallout after the HLS coordination split.
- The simplest still-failing HLS seek repro inside `suite_light`.

What was changed:
- Cleaned up `tests/tests/kithara_hls/source_internal_cases.rs` so it matches the current
  production contract instead of preserving deleted shadow state.
- Removed the test-only `current_segment_index` field from the helper harness.
- Switched the two demand-dedup tests to non-destructive observation of the demand slot
  (`peek()` instead of draining it with `take()`).
- Updated the non-destructive seek-epoch test to reflect the current exact-EOF contract.

What was tested:
- Focused `source_internal_cases` regressions:
  - `commit_seek_landing_uses_decoder_landed_offset_with_anchor_variant`
  - `commit_seek_landing_skips_request_when_landed_segment_is_ready`
  - `set_seek_epoch_is_non_destructive`
  - `test_wait_range_stalled_on_demand_request_is_not_duplicated`
  - `test_wait_range_midstream_switch_repush_is_not_duplicated`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light -- --nocapture`

Result:
- The stale `source_internal_cases` regressions are green again.
- `suite_light` now fails in 7 tests, grouped into two real clusters:
  1. HLS wait/seek timeout cluster:
     - `kithara_hls::deferred_abr::seek_to_segment_boundary_reads_correct_segment_case_3`
     - `kithara_hls::source_seek::hls_stream_seek_to_segment_start_case_3`
     - `kithara_hls::wait_range_contract::seek_burst_then_tail_read_stays_contiguous_disk`
     - `kithara_hls::wait_range_contract::seek_burst_then_tail_read_stays_contiguous_ephemeral`
  2. Player/file lifecycle cluster:
     - `player_resource_repeated_unavailable_mp3_does_not_panic`
     - `player_resource_mp3_reopen_same_cache_keeps_backward_seek_disk`
     - `player_resource_mp3_reopen_same_cache_keeps_backward_seek_ephemeral`

New concrete evidence:
- Focused logging on `kithara_hls::source_seek::hls_stream_seek_to_segment_start_case_3`
  shows the downloader trying to load an init segment for variant 0 and then stalling:
  - `init segment load failed variant=0 error=Segment not found: init segment not found in variant 0 playlist`
- This is a simpler and more concrete HLS repro than the broader heavy-suite failures.

Updated interpretation:
- The current simplest HLS bug is no longer about deleted shadow fields or the generic
  `Timeline` ownership split.
- The immediate repro is: direct seek to a non-zero segment on a playlist without init
  segments should not trigger init loading.
- The player/file regressions remain separate and should stay untouched until the simpler
  HLS cluster is isolated or fixed.

## Entry 026
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Whether the current workspace state was actually commit-ready after the stream-role refactor.

What was found:
- Pre-commit was still blocked, but not by runtime regressions.
- `cargo clippy --workspace -- -D warnings` surfaced a follow-up layer of mechanical debt in:
  - `kithara-stream`
  - `kithara-hls`
  - `kithara-audio`
  - one fuzz target
- The issues were local and non-behavioral:
  - missing `#[must_use]`
  - trivial `Default` impl that should be derived
  - `map(...).unwrap_or(...)`
  - explicit safe integer conversions
  - duplicated match arms
  - lock guards held longer than their last use
  - `Stream::len()` missing `is_empty()`

What was changed:
- Cleaned those clippy blockers without changing protocols or adding new abstractions.
- Kept the fixes strictly mechanical so the runtime bug surface remains unchanged.

What was tested:
- `cargo clippy --workspace -- -D warnings`

Result:
- `clippy` is now green for the whole workspace again.

Updated interpretation:
- The current tree is back to a clean structural baseline:
  - integration harness cleanup is in place
  - generic stream-role refactor compiles cleanly under workspace lint policy
- The next meaningful step is again runtime-focused:
  - add the smallest missing HLS regression test for the init-less direct-seek case
  - then run full `cargo test`

## Entry 027
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Whether the simplest HLS timeout could be reduced to planner policy alone.

What was changed:
- Added two focused unit scenarios in `crates/kithara-hls/src/downloader.rs`:
  - `build_demand_plan_initial_nonzero_segment_without_init_does_not_request_init`
  - `build_batch_plans_seek_into_init_less_playlist_starts_at_target_segment`

What was tested:
- `cargo test -p kithara-hls build_demand_plan_initial_nonzero_segment_without_init_does_not_request_init -- --nocapture`
- `cargo test -p kithara-hls build_batch_plans_seek_into_init_less_playlist_starts_at_target_segment -- --nocapture`

Result:
- Both new scenarios are green.

Updated interpretation:
- The current simple HLS failure is not explained by `build_demand_plan()` alone.
- It is also not explained by `build_batch_plans()` alone in the init-less direct-seek case.
- That moves suspicion away from pure plan construction and toward a later runtime path:
  - fetch execution
  - source/downloader interaction after seek
  - or a mismatch between committed layout and what `wait_range()` waits for

## Entry 028
Time: 2026-03-13 Europe/Moscow

What was tested:
- `cargo test`

Result:
- Unit and most integration layers are green.
- `suite_heavy` now fails in 17 tests.

Failure clusters from the full run:
1. HLS non-DRM seek/read timeout cluster:
   - `live_stress_real_stream_seek_read_cache_hls_ephemeral`
   - `stress_seek_lifecycle_with_zero_reset_ephemeral`
   - `stress_random_seek_read_hls_small`
   - `stress_random_seek_read_hls_medium`
   - `stress_random_seek_read_hls_init_small`
   - `stress_random_seek_read_hls_init_medium`
2. HLS DRM/encrypted cluster on top of the same seek/read path:
   - `live_ephemeral_revisit_sequence_regression_drm`
   - `live_ephemeral_small_cache_seek_stress_drm`
   - `live_real_stream_random_seek_prefix_regression_drm`
   - `live_real_stream_seek_resume_native_drm`
   - `live_stress_real_stream_seek_read_cache_drm_ephemeral`
   - `live_stress_real_stream_seek_read_cache_drm_mmap`
   - `stress_seek_during_abr_switch_real_decoder_drm`
   - `stress_random_seek_read_hls_encrypted_small`
   - `stress_random_seek_read_hls_encrypted_medium`
3. Multi-instance fallout:
   - `multi_instance::concurrent_file::four_file_instances`
   - `multi_instance::concurrent_mixed::mixed_four_file_four_hls`

Most useful concrete evidence from the full run:
- The simplest synthetic HLS failures (`stress_random_seek_read_hls_small` and
  `stress_random_seek_read_hls_init_small`) fail with:
  - `source error: Timeout: wait_range budget exceeded`
- Those logs still show:
  - `init segment load failed variant=0 error=Segment not found: init segment not found in variant 0 playlist`
- But the two focused planner tests are already green, so the root cause is later than pure plan construction.
- `live_stress_real_mp3_seek_read_cache_{ephemeral,mmap}` are green in the same full run.
  That weakens the earlier hypothesis that file/mp3 seek is still broadly broken at the stream layer.

Updated interpretation:
- The primary cluster to fix first is now the non-DRM HLS synthetic/ephemeral seek timeout path.
- DRM failures should be treated as secondary amplifications of the same underlying HLS seek/read issue.
- The multi-instance file/mixed failures are likely separate and should stay out of scope until the
  simpler HLS cluster is resolved.

## Entry 029
Time: 2026-03-13 Europe/Moscow

What was investigated:
- The remaining init-less HLS startup path after the earlier planner tests.
- Specifically: whether the planner still injects a fake init load on the very first segment
  (`segment_index == 0`) when the variant has no `EXT-X-MAP`.

What was changed:
- Added two focused downloader tests in `crates/kithara-hls/src/downloader.rs`:
  - `build_demand_plan_initial_segment_zero_without_init_does_not_request_init`
  - `build_batch_plans_initial_segment_zero_without_init_does_not_request_init`
- Updated planner logic so `need_init` is gated by actual variant topology
  (`playlist_state.init_url(variant).is_some()`), not only by seek/switch heuristics.

What was tested:
- `cargo test -p kithara-hls initial_segment_zero_without_init -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_hls::source_seek::hls_stream_seek_to_segment_start_case_3 -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_heavy kithara_hls::stress_seek_random::stress_random_seek_read_hls_small -- --nocapture`

Result:
- The two new unit tests fail before the fix and pass after it.
- The noisy warning
  `init segment load failed variant=0 error=Segment not found: init segment not found in variant 0 playlist`
  disappears from the init-less repros after the fix.
- The actual seek timeout remains.

Updated interpretation:
- Fake init injection on init-less playlists was a real bug and is now covered.
- It was not the root cause of the current seek timeout cluster.
- The remaining primary non-DRM repro is still the seek lifecycle path after seek commit/reconcile,
  not playlist topology or init planning.

## Entry 030
Time: 2026-03-13 Europe/Moscow

What was investigated:
- Player/resource regressions reported from the app:
  - switching from working HLS to MP3 can poison later MP3 playback
  - backward seek on MP3 degrades, and repeated open behaves differently from the first open
  - repeated transition to unavailable MP3 must not crash or poison later playback

What was changed:
- Added two new integration scenarios in `tests/tests/kithara_play/resource_regressions.rs`:
  - `player_worker_hls_then_unavailable_mp3_then_mp3_recovery`
  - `player_worker_hls_then_mp3_reopen_keeps_backward_seek`
- These use the player’s shared worker plus a shared `StoreOptions`, so they cover the
  real transition path more closely than isolated `Resource` tests.
- The HLS side is warmed through `Audio<Stream<Hls>>` with explicit WAV media info,
  because `Resource` cannot supply `media_info` to HLS and therefore cannot open the
  synthetic WAV-over-HLS fixture directly.

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::player_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions -- --nocapture`

Result:
- The new shared-worker HLS→MP3 scenario now reproduces a hard failure:
  - `HangDetector: kithara_audio::pipeline::audio_worker::run_shared_worker_loop no progress for 1s`
  - then the test times out after 10s.
- The older MP3-only regression tests in the same module also time out under the current tree,
  which means the app symptom is now reproducible at integration-test level and is not just a UI-only issue.

Updated interpretation:
- There is a real shared-worker/file-path regression on MP3 that survives outside the app.
- The difference between first and repeated MP3 opens is likely lifecycle/cache/worker related,
  not just player UI logic.
- This regression is now test-backed and can be tracked alongside the HLS timeout cluster.

## Entry 031
Time: 2026-03-13 Europe/Moscow

What was tested:
- `cargo test`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions -- --nocapture`

Result:
- `cargo test` now completes unit layers cleanly and then fails in `suite_heavy` with 16 tests:
  - `kithara_hls::live_stress_real_stream::live_ephemeral_revisit_sequence_regression_drm`
  - `kithara_hls::live_stress_real_stream::live_ephemeral_small_cache_seek_stress_drm`
  - `kithara_hls::live_stress_real_stream::live_real_stream_fixed_seek_window_regression_drm`
  - `kithara_hls::live_stress_real_stream::live_real_stream_random_seek_prefix_regression_drm`
  - `kithara_hls::live_stress_real_stream::live_real_stream_seek_resume_native_drm`
  - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_drm_ephemeral`
  - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_drm_mmap`
  - `kithara_hls::live_stress_real_stream::live_stress_real_stream_seek_read_cache_hls_ephemeral`
  - `kithara_hls::stress_seek_abr::stress_seek_during_abr_switch_real_decoder_drm`
  - `kithara_hls::stress_seek_lifecycle::stress_seek_lifecycle_with_zero_reset_ephemeral`
  - `kithara_hls::stress_seek_random::stress_random_seek_read_hls_encrypted_medium`
  - `kithara_hls::stress_seek_random::stress_random_seek_read_hls_encrypted_small`
  - `kithara_hls::stress_seek_random::stress_random_seek_read_hls_init_medium`
  - `kithara_hls::stress_seek_random::stress_random_seek_read_hls_init_small`
  - `kithara_hls::stress_seek_random::stress_random_seek_read_hls_medium`
  - `kithara_hls::stress_seek_random::stress_random_seek_read_hls_small`
- `suite_light::kithara_play::resource_regressions` separately fails in 7 tests:
  - `player_resource_mp3_reopen_same_cache_keeps_backward_seek_disk`
  - `player_resource_mp3_reopen_same_cache_keeps_backward_seek_ephemeral`
  - `player_resource_repeated_unavailable_mp3_does_not_panic`
  - `player_worker_hls_then_mp3_reopen_keeps_backward_seek_disk`
  - `player_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral`
  - `player_worker_hls_then_unavailable_mp3_then_mp3_recovery_disk`
  - `player_worker_hls_then_unavailable_mp3_then_mp3_recovery_ephemeral`

Most useful concrete evidence:
- MP3/resource regressions consistently hit:
  - `HangDetector: kithara_audio::pipeline::audio_worker::run_shared_worker_loop no progress for 1s`
  - sometimes also `kithara_audio::pipeline::audio::recv_outcome_blocking no progress for 1s`
  - then 10s test timeout
- HLS heavy failures split into at least three sub-signatures:
  1. random seek timeout:
     - `source error: Timeout: wait_range budget exceeded`
  2. lifecycle deadlock:
     - `recv_outcome_blocking` / `run_shared_worker_loop` hang detector
  3. DRM early-stop / cancel after seek:
     - `DRM random seek stopped early at idx=0`
     - `source error: Cancelled`

Updated interpretation:
- The current tree has two genuinely separate failure clusters:
  1. file/mp3/shared-worker lifecycle regression
  2. HLS seek/read regression family
- The simpler next target is the mp3-only regression
  `player_resource_mp3_reopen_same_cache_keeps_backward_seek_{ephemeral,disk}`,
  because it avoids DRM, HLS, and cross-protocol transitions while still reproducing the same hang detector.

## Entry 032
Time: 2026-03-13 Europe/Moscow

What was retested on current HEAD:
- `cargo test`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions -- --nocapture`

Current results:
- `cargo test` still fails in `suite_heavy` with 16 tests, but the exact set changed. Newly green on current HEAD compared to the previous inventory: `live_real_stream_fixed_seek_window_regression_drm`, `stress_seek_random::stress_random_seek_read_hls_medium`. Newly red on current HEAD: `live_ephemeral_revisit_sequence_regression_hls`, `multi_instance::concurrent_mixed::mixed_two_file_two_hls`.
- `suite_light::kithara_play::resource_regressions` now has only 2 failures, both ephemeral-only:
  - `player_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral`
  - `player_worker_hls_then_unavailable_mp3_then_mp3_recovery_ephemeral`
- The disk variants of those same shared-worker tests are now green, as are the MP3-only resource regressions.

Most useful narrowing:
- The remaining light failures are not generic mp3 seek bugs anymore.
- They require all of:
  - shared player worker
  - prior HLS warmup
  - ephemeral asset store
- This isolates the next target to ephemeral lifecycle / residency / cleanup around shared-worker transitions, not basic file demand or standalone mp3 seek.

Immediate next target:
- Work only on `player_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral` and `player_worker_hls_then_unavailable_mp3_then_mp3_recovery_ephemeral` before touching HLS heavy failures again.

## Entry 033
Time: 2026-03-13 20:28:59 MSK

What was changed:
- Kept only clean regression scenarios in `tests/tests/kithara_play/resource_regressions.rs`.
- Removed temporary `eprintln!` stage markers after the failure point had already been localized.
- Added two reduced shared-worker scenarios that avoid UI noise:
  - `shared_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral`
  - `sequential_hls_warmup_does_not_poison_next_ephemeral_session`

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::shared_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::sequential_hls_warmup_does_not_poison_next_ephemeral_session -- --nocapture`

Result:
- `shared_worker_hls_then_mp3_reopen_keeps_backward_seek_ephemeral` passes in isolation.
- `sequential_hls_warmup_does_not_poison_next_ephemeral_session` fails reliably with:
  - `HangDetector: kithara_audio::pipeline::audio::recv_outcome_blocking no progress for 1s`
  - `HangDetector: kithara_audio::pipeline::audio_worker::run_shared_worker_loop no progress for 1s`

Updated interpretation:
- The remaining light-cluster regression is not MP3-specific anymore.
- A first HLS warmup/teardown in-process can poison a later ephemeral HLS session even without any MP3 transition.
- The next debugging target should be shared-worker / HLS / ephemeral cleanup between sessions, not file seek behavior.

## Entry 034
Time: 2026-03-13 Europe/Moscow

What was added:
- Three narrower regression scenarios in `tests/tests/kithara_play/resource_regressions.rs`:
  - `sequential_hls_warmup_drop_only_does_not_poison_next_ephemeral_session`
  - `sequential_hls_read_only_session_does_not_poison_next_ephemeral_session`
  - `sequential_hls_stream_sessions_do_not_poison_next_ephemeral_session`

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::sequential_hls_warmup_drop_only_does_not_poison_next_ephemeral_session -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::sequential_hls_read_only_session_does_not_poison_next_ephemeral_session -- --nocapture`
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_play::resource_regressions::sequential_hls_stream_sessions_do_not_poison_next_ephemeral_session -- --nocapture`

Result:
- `sequential_hls_warmup_drop_only_does_not_poison_next_ephemeral_session` fails with the same hang detector pair as the original repro.
- `sequential_hls_read_only_session_does_not_poison_next_ephemeral_session` also fails with the same hang detector pair.
- `sequential_hls_stream_sessions_do_not_poison_next_ephemeral_session` fails without `Audio` and without `AudioWorker`; the second session times out with:
  - `source error: Timeout: wait_range budget exceeded: range=0..4096 ...`

Updated interpretation:
- The primary remaining reduced repro is no longer in the audio worker.
- `shutdown()` is not required for the failure.
- A first-session `seek()` is not required for the failure.
- The failure reproduces at the `Stream<Hls>` layer, so the next investigation should focus on HLS source/downloader/backend lifecycle between sequential sessions in one process.

## Entry 035
Time: 2026-03-13 Europe/Moscow

What was changed:
- Migrated the remaining obvious asset-store integration write paths from `open_resource*()` to `acquire_resource*()` in:
  - `tests/tests/kithara_assets/*`
  - `crates/kithara-hls/src/internal.rs`

What was tested:
- `cargo test --manifest-path tests/Cargo.toml --test suite_light kithara_assets -- --nocapture`

Result:
- All `kithara_assets` light integration tests are green on the new access contract:
  - `78 passed; 0 failed`

Updated interpretation:
- The broad `kithara_assets` fallout after tightening write access was mostly migration debt in tests/fixtures, not a new runtime regression.
- The remaining failures are now outside this migration slice and should be treated as separate HLS/file runtime bugs.
