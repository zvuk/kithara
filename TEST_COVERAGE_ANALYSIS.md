# Test Coverage Analysis

## Overview

The kithara workspace contains **11 crates** with **~84 source files** and approximately
**479 test functions** split between inline unit tests (274) and integration tests (205).
The test-to-source ratio is healthy overall, but coverage is uneven — a few crates
carry the bulk of the testing while others have significant blind spots.

---

## Current Coverage Summary

| Crate | Inline Tests | Integration Tests | Assessment |
|-------|-------------|-------------------|------------|
| **kithara-decode** | 90 | ~30 | Strong |
| **kithara-net** | 40 | ~35 | Strong |
| **kithara-audio** | 32 | 0 | Moderate (no integration tests) |
| **kithara-hls** | 19 | ~60 | Strong |
| **kithara-assets** | 15 | ~30 | Good |
| **kithara-storage** | 12 | ~20 | Good |
| **kithara-abr** | 11 | ~6 | Moderate |
| **kithara-stream** | 9 + 21 (test dir) | ~10 | Moderate |
| **kithara-file** | 9 | ~10 | Weak |
| **kithara-bufpool** | 8 | 0 | Moderate |
| **kithara** (facade) | 8 | 0 | Weak |

---

## Priority 1 — Critical Gaps (Zero or Near-Zero Coverage)

### 1. `kithara/src/resource.rs` — Public facade is untested

The `Resource` struct is the main user-facing API. It has **15 public methods** and
**zero tests** — no unit tests, no integration tests.

**Untested surface:**
- `Resource::new()` — auto-detection of file vs. HLS
- `Resource::from_reader()` — custom PcmReader path
- `Resource::read()` / `read_planar()` — PCM output
- `Resource::seek()`, `position()`, `duration()`
- `Resource::preload()` — preload first chunk
- `Resource::subscribe()` — event forwarding
- Feature-gated `from_file()` / `from_hls()` constructors

**Risk:** Regressions in the top-level API go undetected. A consumer could call
`Resource::new("https://example.com/stream.m3u8")` and hit an unexercised code path
through auto-detection, event forwarding, or type erasure.

**Recommendation:** Add integration tests that construct `Resource` from both file and
HLS sources, exercise `read()`, `seek()`, `spec()`, `is_eof()`, and verify event
delivery through `subscribe()`.

---

### 2. `kithara-file/src/session.rs` — Session management is untested

This 382-line file contains `FileSource`, `Progress`, and `SharedFileState` — all
with **zero direct tests**. The on-demand range-request flow, read/download position
tracking, and reader/downloader synchronization are never exercised.

**Untested surface:**
- `Progress` — atomic read/download position tracking, notification signaling
- `FileSource::wait_range()` — core blocking logic for progressive download
- `SharedFileState::request_range()` / `pop_range_request()` — demand-driven fetch
- Deadlock prevention between reader and downloader

**Risk:** Concurrency bugs in position tracking or range-request queueing are unlikely
to surface through higher-level tests that treat the file module as a black box.

**Recommendation:** Add unit tests for `Progress` (atomic state transitions, signal
notification) and `FileSource::wait_range()` (committed vs. active resource, range
request trigger). These can use a mock `StorageResource`.

---

### 3. `kithara-audio` — No integration tests, pipeline logic untested

The audio crate has 32 inline tests, mostly in `resampler.rs` (14) and
`stream_source.rs` (6). But the core pipeline orchestration has no coverage:

**Untested surface:**
- `Audio` struct (`pipeline/audio.rs`, 920 lines) — the main audio pipeline
- `AudioConfig` (`pipeline/config.rs`, 154 lines) — pipeline configuration
- `SourceReader` (`reader.rs`, 102 lines) — sync `Read + Seek` adapter
- `pipeline/worker.rs` (193 lines) — worker thread lifecycle
- `AudioSyncReader` (`sync.rs`, 139 lines) — rodio `Source` adapter

**Risk:** The pipeline connects decoding, resampling, and effects into a unified audio
output. This composition layer has no dedicated tests, so interactions between
components (e.g., sample-rate mismatch between decoder and resampler, or EOF
propagation through the pipeline) go untested.

**Recommendation:** Add integration tests that construct an `Audio` pipeline from a
test fixture, verify PCM output, and test seek/EOF behavior end-to-end. The
`SourceReader` should have unit tests for its `Read` and `Seek` trait implementations.

---

## Priority 2 — Incomplete Error Path Coverage

### 4. `StreamError<E>` — 5 of 6 variants untested

The generic stream error type has variants that are never triggered in any test:

| Variant | Tested? |
|---------|---------|
| `SeekNotSupported` | No |
| `InvalidSeek` | No |
| `UnknownLength` | No |
| `ChannelClosed` | No |
| `WriterJoin(String)` | No |
| `Source(E)` | Partially (indirect) |

**Recommendation:** Add tests in `kithara-stream` that:
- Attempt to seek on a non-seekable source → `SeekNotSupported`
- Seek past valid range → `InvalidSeek`
- Seek when content length is unknown → `UnknownLength`
- Drop the consumer side of the channel → `ChannelClosed`
- Simulate a writer task panic → `WriterJoin`

---

### 5. `DecodeError` — 5 of 8 variants untested

| Variant | Tested? |
|---------|---------|
| `UnsupportedCodec(AudioCodec)` | No |
| `UnsupportedContainer(ContainerFormat)` | No |
| `SeekFailed(String)` | No |
| `SeekError(String)` | No |
| `ProbeFailed` | No |
| `Io`, `InvalidData`, `Backend` | Yes |

**Recommendation:** Add tests with unsupported or corrupt audio fixtures that trigger
codec/container rejection and probe failure. Test seeking to invalid positions in the
decoder.

---

### 6. `HlsError` — 5 dead variants

These variants are defined but never returned anywhere in the codebase:

- `NoSuitableVariant` — ABR found no matching variant
- `Abr(String)` — generic ABR error
- `OfflineMiss` — offline mode cache miss
- `Unimplemented`
- `Driver(String)` — has an `#[ignore]`d test

**Recommendation:** Either add tests and code paths that produce these errors, or
remove unused variants. Dead error variants inflate the API surface without providing
value. If offline mode and driver pipelines are planned features, add `todo!()` tests
to track them.

---

### 7. `AssetsError` — 6 of 8 variants untested

| Variant | Tested? |
|---------|---------|
| `InvalidKey` | No |
| `Bincode` / `BincodeDecode` | No |
| `Canonicalization(String)` | No |
| `InvalidUrl(String)` | No |
| `MissingComponent(String)` | No |
| `Io`, `Storage` | Yes |

**Recommendation:** Test resource key validation with invalid inputs (`InvalidKey`).
Corrupt index metadata to trigger bincode decode failures. Test URL canonicalization
with edge-case paths (relative URLs, missing components).

---

## Priority 3 — Thin Coverage in Core Logic

### 8. `Writer::with_offset()` — range-request resume path untested

`Writer<E>` supports starting at a non-zero byte offset (for HTTP Range requests and
download resumption). This path is never tested. The `OffsetOverflow` error variant
(u64 overflow when adding offset) is also never triggered.

**Recommendation:** Add tests that create a `Writer::with_offset(n)` and verify it
writes to the correct storage position. Add an overflow test with `u64::MAX`.

---

### 9. `kithara-bufpool` — Cross-shard fallback and saturation untested

The pool has 8 tests covering basic get/reuse, but several important paths are missing:

- **Cross-shard fallback:** When a thread's shard is empty, the pool tries other
  shards. Never tested.
- **Shard saturation:** When a shard exceeds capacity, buffers are dropped instead of
  returned. Never tested.
- **`Pooled::into_inner()` / `PooledOwned::into_inner()`:** Extract-without-return
  behavior never tested.
- **Multi-threaded contention:** No concurrent access tests.

**Recommendation:** Add multi-threaded tests that exhaust one shard and verify
cross-shard fallback. Test capacity limits. Test `into_inner()` to verify the buffer
is not returned to the pool.

---

### 10. `kithara-abr` — Down-switch hysteresis and edge cases

The ABR controller has 17 tests with good algorithmic coverage, but gaps remain:

- **Down-switch hysteresis:** Only basic cases tested; rapid oscillation prevention
  is not verified.
- **`apply()` method:** Never tested. This applies decisions to the controller state.
- **Empty variants list:** Edge case where no variants are available.
- **Buffer level edge cases:** Negative values, zero buffer.
- **Float precision at switching boundaries:** Throughput exactly at variant bitrate.

**Recommendation:** Add parametrized tests for boundary conditions around the
hysteresis ratio. Test `apply()` round-trips. Test with empty and single-element
variant lists.

---

## Priority 4 — Structural Improvements

### 11. Cancellation token coverage is inconsistent

`kithara-storage` tests cancellation thoroughly (pre-cancelled tokens, cancellation
during I/O). But `kithara-stream`, `kithara-file`, and `kithara-hls` do not test
cancellation during active streaming operations.

**Recommendation:** Add cancellation tests for `Writer::run()`, `FileSource::wait_range()`,
and HLS segment fetching. Verify clean shutdown and resource cleanup.

---

### 12. No property-based or fuzz testing

The codebase uses `rstest` parametrization extensively, which is good for known cases.
However, complex parsers (HLS playlists, audio probing) and numeric logic (ABR
throughput estimation, buffer position tracking) would benefit from property-based
testing with `proptest` or similar.

**Candidates for property-based tests:**
- HLS playlist parsing with randomly malformed input
- ABR throughput estimation with random sample sequences
- Buffer pool with random get/return patterns across threads
- Storage `write_at` / `read_at` with random offset sequences

---

### 13. One `#[ignore]`d test needs resolution

`tests/tests/kithara_hls/driver_test.rs` contains `test_driver_after_fixing_Driver_1`
which is marked `#[ignore]`. This appears to be tracking a known bug. It should either
be fixed and un-ignored, or converted to a tracked issue.

---

## Summary of Recommendations

| Priority | Area | Action |
|----------|------|--------|
| **P1** | `Resource` facade | Add end-to-end integration tests |
| **P1** | `FileSource` / `Progress` / `SharedFileState` | Add unit tests for session management |
| **P1** | `Audio` pipeline | Add integration tests for pipeline composition |
| **P2** | `StreamError` variants | Test all 5 untested error paths |
| **P2** | `DecodeError` variants | Test unsupported codec/container and seek failures |
| **P2** | `HlsError` dead variants | Test or remove 5 unused variants |
| **P2** | `AssetsError` variants | Test key validation, bincode, URL edge cases |
| **P3** | `Writer::with_offset()` | Test range-request resume and offset overflow |
| **P3** | Buffer pool edge cases | Test cross-shard fallback, saturation, `into_inner()` |
| **P3** | ABR boundary conditions | Test hysteresis, `apply()`, empty variants |
| **P4** | Cancellation tokens | Extend cancellation tests to stream/file/hls |
| **P4** | Property-based testing | Add `proptest` for parsers and numeric logic |
| **P4** | Ignored test | Fix or track `driver_test` |
