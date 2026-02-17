# Test Coverage Analysis

## Current State

The workspace contains **15 crates** with approximately **505 unit tests** and **186+ integration tests** (plus ~226 rstest-parameterized scenarios). The test infrastructure uses rstest, tokio::test, unimock, serial_test, and wasm-bindgen-test. Coverage is generated via cargo-tarpaulin.

No property-based testing (proptest/quickcheck) or fuzzing targets exist.

### Per-Crate Test Summary

| Crate | Unit Tests | Integration Tests | Coverage Quality |
|-------|-----------|-------------------|-----------------|
| kithara-abr | 19 | 4 (in kithara-hls) | Good core logic, gaps in edge cases |
| kithara-assets | 39 | 41 | Good, gaps in eviction/concurrency |
| kithara-audio | 45 | 2 | Good happy paths, gaps in errors |
| kithara-bufpool | 13 | 0 | Good |
| kithara-decode | 84 | 6 | Good probing/types, weak on decode pipeline |
| kithara-drm | 8 | 0 | **Severely undertested** |
| kithara-events | 10 | 0 | **Severely undertested** |
| kithara-file | 20 | 7 | Weak — core downloader untested |
| kithara-hls | 39 | 36 | Good |
| kithara-net | 41 | 50 | Good, gaps in error scenarios |
| kithara-platform | 0 | 0 | Platform-specific, N/A |
| kithara-storage | 52 | 34 | Good |
| kithara-stream | 24 | 25 | Weak on backend/writer internals |
| kithara-wasm | 0 | 0 | WASM-only |
| kithara (main) | 16 | 0 | Weak — config conversion untested |

---

## Priority 1: Critical Gaps

These areas have complex logic with no or minimal test coverage and are most likely to harbor bugs.

### 1.1 kithara-drm — DRM Decryption (3 tests for security-critical code)

**Location:** `crates/kithara-drm/src/`

The entire crate has only 8 test cases for AES-128-CBC decryption, which is security-critical code protecting media content.

**Missing tests:**
- All 4 `DrmError` variants are never constructed or tested
- PKCS7 padding edge cases: 1-byte plaintext, block-aligned plaintext (should add full padding block), 15-byte plaintext
- IV chaining verification: explicit check that `ctx.iv` updates on intermediate chunks but not on final chunks
- Output buffer boundary conditions
- Corrupted ciphertext behavior (CBC doesn't authenticate; verify plaintext is corrupted, not panicked)
- Known test vectors from NIST/RFC standards

**Why it matters:** Silent decryption bugs produce garbled audio without any error, making them hard to catch in production.

### 1.2 kithara-file — FileDownloader State Machine (0 tests)

**Location:** `crates/kithara-file/src/downloader.rs`

The `FileDownloader` is the core download orchestrator managing sequential download, gap-filling, on-demand range requests, and backpressure. It has **1 unit test** covering a single phase transition. The following critical paths have zero coverage:

**Missing tests:**
- `FileDownloader::plan()` — state machine with sequential, gap-filling, and complete phases
- `FileDownloader::step()` — sequential streaming with chunk/error/EOF handling
- `FileDownloader::commit()` — resource commit with 3 distinct flows and multiple error branches
- `FileDownloader::should_throttle()` — backpressure logic
- `FileIo::fetch()` — range request creation and network error handling
- `FileCoverage` — all 6 methods (mark, is_complete, next_gap, gaps, total_size, flush)
- `FileSource::wait_range()` — on-demand seek/range logic (30+ lines, potential deadlocks)
- `File::create_local()` and `File::create_remote()` — initialization with multiple error paths

**Why it matters:** The downloader handles partial downloads, resume, gap-filling, and backpressure. Bugs here cause stalled playback, incomplete downloads, or data corruption.

### 1.3 kithara-stream — Backend Event Loop (1 test for 400+ lines)

**Location:** `crates/kithara-stream/src/backend.rs`

The `Backend::run_downloader()` function is the async event loop that coordinates batch fetching, demand processing, backpressure, and cancellation. It has 1 test.

**Missing tests:**
- Batch parallel fetch with partial failure (some fetches succeed, some fail)
- Backpressure wait loop (`while should_throttle`) with demand signal interruption
- Cancellation during batch fetch, during plan(), during step(), during commit
- Demand priority — demand arrives between batch commits
- Multiple queued demands
- Yield interval behavior

### 1.4 kithara-stream — Writer Error Paths (0 unit tests)

**Location:** `crates/kithara-stream/src/writer.rs`

The `Writer` struct handles async byte pumping from network to storage. It has 6 integration tests but 0 unit tests, and no error path is exercised.

**Missing tests:**
- Source stream error → should call `resource.fail()`
- Storage write error with resource status check (already-committed race)
- Empty source stream (immediate EOF)
- Cancellation before first chunk
- `OffsetOverflow` at various boundary values

### 1.5 kithara-stream — Fetch/Epoch Validation (0 tests)

**Location:** `crates/kithara-stream/src/fetch.rs`

The `Fetch<C>` wrapper and `EpochValidator` have no tests at all. Epoch validation prevents stale data from ABR variant switches from being committed.

**Missing tests:**
- `EpochValidator::is_valid()` with matching and mismatching epochs
- `EpochValidator::next_epoch()` incrementing
- `Fetch::into_inner()`, `is_eof()`, `epoch()` accessors

---

## Priority 2: Significant Gaps

These areas have partial coverage but miss important error handling or edge cases.

### 2.1 kithara-events — Event Variant Coverage (86% untested)

**Location:** `crates/kithara-events/src/`

Only 3 of 21 event variants are tested in conversion tests. The `EventBus` has 5 tests covering basic pub/sub but no concurrency tests despite using `tokio::sync::broadcast`.

**Missing tests:**
- All `FileEvent` variants except `DownloadComplete` and `EndOfStream` (4 untested)
- 9 of 11 `HlsEvent` variants untested (VariantsDiscovered, SegmentStart, SegmentComplete, ThroughputSample, DownloadProgress, DownloadError, PlaybackProgress, Error)
- 3 of 4 `AudioEvent` variants untested (FormatDetected, FormatChanged, SeekComplete)
- `EventBus::new(0)` — capacity floor (code does `.max(1)`)
- Concurrent publishers from multiple threads
- Subscriber lag under concurrent load

### 2.2 kithara-decode — Symphonia Decode Pipeline

**Location:** `crates/kithara-decode/src/symphonia.rs`

The factory/probing logic is well-tested (39 tests), but the actual decode pipeline (`SymphoniaInner::next_chunk()`) has significant untested branches:

**Missing tests:**
- `ResetRequired` error recovery (decoder reset loop)
- `UnexpectedEof` special handling
- Packet skipping for non-audio tracks
- Zero-sample chunk skipping
- `calculate_duration()` time base edge cases
- Direct format reader creation vs probe-based creation
- Seek accuracy with frame offset calculation

### 2.3 kithara-abr — Concurrent Access and Edge Cases

**Location:** `crates/kithara-abr/src/`

Core switching logic is well-tested. Gaps are in edge cases:

**Missing tests:**
- Concurrent `decide()` + `apply()` from different threads (uses `Arc<AtomicUsize>`)
- Manual mode with out-of-bounds variant index
- Zero-bandwidth variant in the variant list
- All variants exceeding available throughput (fallback to lowest)
- Safety factor extreme values (0.1, 10.0)
- Cache hit followed by network samples (EWMA transition)

### 2.4 kithara-assets — Eviction and Concurrency

**Location:** `crates/kithara-assets/src/`

Good decorator-chain test coverage, but gaps in:

**Missing tests:**
- `EvictAssets::record_asset_bytes()` and `check_and_evict_if_over_limit()` — public API, 0 tests
- Concurrent lease guard drops during eviction
- Processing failures during `commit()` with partial data
- Cache miss TOCTOU race (check → open between concurrent callers)
- Builder pattern combinations (various enable/disable permutations)
- `ResourceKey::from_url()` — untested

### 2.5 kithara (main crate) — Config Conversion

**Location:** `crates/kithara/src/config.rs`

**Missing tests:**
- `ResourceConfig::into_file_config()` — field mapping, extension extraction, store/net options
- `ResourceConfig::into_hls_config()` — URL validation, error on local path, base URL resolution
- Builder methods: `with_name()`, `with_event_channel_capacity()`, `with_thread_pool()`
- Resource event forwarding: broadcast channel overflow (`RecvError::Lagged`)
- rodio `Source` trait implementation

---

## Priority 3: Structural Improvements

### 3.1 Add Property-Based Testing

Several components would benefit from property-based testing with `proptest`:

- **kithara-drm**: Arbitrary plaintext → encrypt → decrypt roundtrip for any length
- **kithara-storage**: Arbitrary write_at/read_at offset pairs maintain data integrity
- **kithara-abr**: Arbitrary throughput sample sequences produce monotonically reasonable decisions
- **kithara-stream media.rs**: Arbitrary HLS codec strings parsed correctly or return None

### 3.2 Add Fuzz Targets

High-value fuzz targets:

- **kithara-drm**: `aes128_cbc_process_chunk()` with arbitrary ciphertext + IV
- **kithara-decode factory**: `CodecSelector::select()` with arbitrary hint combinations
- **kithara-hls parsing**: Playlist parsing with malformed M3U8 input
- **kithara-net**: HTTP response parsing with malformed headers

### 3.3 Architecture Test Stubs Need Implementation

`crates/kithara-stream/tests/architecture_progressive_download.rs` documents 10 critical architectural requirements as no-op test functions. These should be converted to executable tests covering:

- Resource state ambiguity (downloaded vs committed)
- Writer StreamEnded vs Committed semantics
- Downloader lifecycle phases
- On-demand request mechanism
- Partial download resumption

### 3.4 Missing Cross-Crate Integration Tests

- **File download end-to-end**: `kithara-net` → `kithara-file` → `kithara-storage` with simulated network failures mid-download
- **HLS with DRM**: `kithara-hls` → `kithara-drm` → `kithara-assets` decryption during segment fetch
- **Decode pipeline**: `kithara-stream` → `kithara-decode` with format changes (ABR variant switch)
- **Full stack**: `kithara` Resource → Audio with seek, format detection, and event propagation

### 3.5 Error Path Testing Pattern

Many crates test happy paths thoroughly but skip error paths entirely. A systematic approach:

1. For each `Result`-returning public function, add at least one test that triggers `Err`
2. For each error enum, verify `Display` output and `From` conversions
3. For network-dependent code, test with `MockNet` returning errors at various stages
4. For storage-dependent code, test with failing resources

---

## Recommended Implementation Order

| # | Area | Effort | Impact |
|---|------|--------|--------|
| 1 | kithara-file downloader state machine | Medium | High — core download reliability |
| 2 | kithara-drm padding/chaining edge cases | Low | High — silent corruption risk |
| 3 | kithara-stream backend event loop | Medium | High — streaming reliability |
| 4 | kithara-stream writer error paths | Low | Medium — resource leak risk |
| 5 | kithara-stream fetch/epoch validation | Low | Medium — stale data risk |
| 6 | kithara-events variant coverage | Low | Low — correctness validation |
| 7 | kithara-decode Symphonia pipeline | Medium | Medium — decode error recovery |
| 8 | kithara-abr concurrent access | Low | Medium — thread safety |
| 9 | kithara-assets eviction | Medium | Medium — disk space management |
| 10 | Property-based tests for drm/storage | Medium | High — broad coverage |
| 11 | Architecture test stubs | High | High — design contract validation |
| 12 | Cross-crate integration tests | High | High — end-to-end reliability |
