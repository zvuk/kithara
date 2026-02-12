# Test Coverage Gap Analysis

Analysis of the kithara workspace test coverage, identifying areas where tests
should be improved. Organized by priority.

## Workspace Overview

| Crate | Prod Lines | Unit Tests | Integration Tests | Test Ratio | Rating |
|-------|-----------|------------|-------------------|------------|--------|
| kithara-abr | 695 | 19 | — | 1.18:1 | Good |
| kithara-net | 717 | ~15 | 20+ | 1.25:1 | Good |
| kithara-bufpool | 685 | 13 | — | — | Good |
| kithara-audio | 2,564 | ~60 | — | 0.89:1 | Mixed |
| kithara-assets | 2,106 | 21 | 8 files | 0.38:1 | Mixed |
| kithara-decode | 3,077 | ~98 | 8 files | 0.40:1 | Mixed |
| kithara (main) | 941 | 17 | — | 0.36:1 | Gaps |
| kithara-storage | 670 | 12 | 2 files | 0.36:1 | Gaps |
| kithara-hls | 2,722 | 8 | 26 files | 0.34:1 | Gaps |
| kithara-file | 1,032 | 18 | 3 files | 0.25:1 | Gaps |
| kithara-stream | 1,056 | 12 | 4 files | 0.20:1 | Gaps |

---

## P0 — Critical Gaps

### 1. `kithara-audio`: Worker loop is completely untested

**File:** `crates/kithara-audio/src/pipeline/worker.rs`

The main audio processing worker loop (`run_audio_loop()`) and all its helpers
have zero test coverage. This is the core of the audio pipeline — it processes
commands, applies effects, handles backpressure, manages EOF state, and
coordinates preloading.

**Untested functions:**
- `run_audio_loop()` — the main decode-and-send loop
- `drain_commands()` — command processing (seek, preload)
- `send_with_backpressure()` — backpressure during PCM send
- `apply_effects()` — effects chain application
- `flush_effects()` — effects flush at EOF
- `reset_effects()` — effects reset after seek

**Suggested tests:**
- Worker processes chunks and sends them through channel
- Seek command resets decoder and effects
- Cancellation token stops the loop
- Backpressure pauses when consumer is slow
- EOF detected and signaled properly
- Preload notification triggers at correct threshold

---

### 2. `kithara-hls`: Encryption/key management untested

**File:** `crates/kithara-hls/src/keys.rs` (192 lines, 0 tests)

AES-128 decryption and key management have no unit tests. This is
security-critical code.

**Untested functions:**
- `decrypt()` — AES-128 CBC decryption
- `derive_iv()` — IV derivation from sequence number
- `decrypt_segment()` — segment-level decryption dispatch
- `get_raw_key()` — key fetching with query params and headers
- `resolve_key_url()` — relative vs absolute URL resolution
- `process_key()` — key processor callback invocation

**Suggested tests:**
- Known-answer test for AES-128 CBC decryption
- IV derivation produces correct 16-byte value from sequence number
- Invalid key length (not 16 bytes) returns error
- Malformed key URL handling
- Key processor callback receives correct data
- Sample-AES vs AES-128 dispatch

---

### 3. `kithara-decode`: Apple decoder has ~5% coverage

**File:** `crates/kithara-decode/src/apple.rs` (1,334 lines, 10 tests)

This file is 31% of the entire decode crate and has near-zero functional test
coverage. All FFI interactions, the decoding loop, and seeking logic are
untested.

**Untested functions:**
- `AppleInner::new()` — FFI setup with AudioFileStream
- `AppleInner::next_chunk()` — main decoding loop
- `AppleInner::feed_parser()` — parser feeding
- `AppleInner::seek()` — seeking with byte offset estimation
- `estimate_byte_offset()` — duration/bitrate calculation
- All FFI callbacks (property_listener, packets, converter_input)

**Suggested tests (where platform allows):**
- Construction with known WAV/MP3 data
- Decoding produces expected PCM output
- Seek to known position
- Feed parser with truncated/corrupt data
- VBR vs CBR packet handling

---

### 4. `kithara-stream`: Core types have no tests

**Files:** `backend.rs`, `stream.rs`, `fetch.rs` — all 0 tests

The `Stream<T>` wrapper (184 lines), `Backend` task spawner (54 lines), and
`Fetch`/`EpochValidator` (74 lines) have zero tests despite being foundational
types.

**Untested in `stream.rs`:**
- `Stream::new()`, `Stream::from_source()`
- `take_events_rx()` — returns `Some` once, then `None`
- `poll_format_change()` / `signal_format_change()`
- `media_info()` — local-then-source fallback
- `Read` and `Seek` trait implementations

**Untested in `fetch.rs`:**
- `EpochValidator::next_epoch()` / `is_valid()`
- `Fetch::is_eof()`, `Fetch::epoch()`

**Untested in `backend.rs`:**
- Task spawning and cancellation
- Downloader completion handling

**Suggested tests:**
- `Stream::from_source()` with a mock source, verify Read/Seek work
- `take_events_rx()` returns `Some` first call, `None` second
- `EpochValidator` increments and rejects stale epochs
- `Backend` cancellation stops the downloader task

---

## P1 — Important Gaps

### 5. `kithara-hls`: Downloader internals (650 lines, 0 unit tests)

**File:** `crates/kithara-hls/src/downloader.rs`

The HLS segment downloader has no unit tests. Integration tests cover happy
paths, but can't easily isolate:

- `fetch_segment()` — 217-line method handling variant switches, init segments,
  byte offset calculations, midstream switch detection
- `populate_cached_segments()` — cache pre-population with offset arithmetic
- `calculate_variant_metadata()` — HEAD request batching
- `make_abr_decision()` — ABR state machine transitions
- `record_throughput()` — throughput sample filtering
- Backpressure and request queue management

### 6. `kithara-file`: On-demand range fetch untested

**File:** `crates/kithara-file/src/inner.rs`

- `FileDownloader::fetch_range()` — HTTP range request execution, writer with
  offset initialization, progress updates — all untested
- `FileDownloader::step()` backpressure logic — look-ahead threshold, reader
  advance notification loop
- Commit failure handling in `step()`
- `FileSource::wait_range()` — on-demand download triggering, progress tracking

### 7. `kithara-file` / `kithara-hls`: Event emission never verified

No test in either crate verifies that the correct events are emitted during
operations. Events are defined (`FileEvent`, `HlsEvent`) but tests don't
subscribe to event channels or assert on event sequences.

**Suggested tests:**
- Download progress events emitted during streaming
- Download complete event on finish
- Error events on failure
- Playback progress events on reads
- HLS variant switch events

### 8. `kithara-hls`: Config builders (259 lines, 0 tests)

**File:** `crates/kithara-hls/src/config.rs`

`HlsConfig` and `KeyOptions` have no builder method tests. Since this is the
primary user-facing entry point, defaults and builder behavior should be
verified.

### 9. `kithara (main)`: Config conversion and rodio integration

**Files:** `crates/kithara/src/config.rs`, `crates/kithara/src/rodio_impl.rs`

- `into_file_config()` / `into_hls_config()` conversion methods — untested
- All builder methods on `ResourceConfig` — untested
- Rodio `Source` trait implementation — entirely untested
- `preload_chunks` clamping behavior

### 10. `kithara-audio`: Pipeline config and rodio adapter

**Files:** `pipeline/config.rs`, `rodio/sync.rs`, `rodio/source_impl.rs`

- `AudioConfig` builder methods — untested
- `expected_output_spec()` — critical for resampling setup, untested
- `AudioSyncReader` — blocking fill_buffer, rodio Source trait — untested

---

## P2 — Moderate Gaps

### 11. Error type coverage

Several error types have untested variants and conversion paths:

| Error Type | Untested Variants |
|-----------|-------------------|
| `StorageError` | `Io`, `Mmap`, `NotCommitted` |
| `SourceError` | All 4 variants (entire type) |
| `HlsError` | `Net`, `Assets`, `Storage`, `Writer`, `SegmentNotFound`, `KeyProcessing` |
| `KeyError` | All variants (entire type); `KeyNotFound` is dead code |
| `AssetsError` | `Io`, `Storage` conversions |

**Additionally:** No tests verify multi-level error propagation chains
(e.g., `HlsError::Storage(StorageError::Io(io::Error))`).

### 12. `kithara-decode`: Factory and metadata gaps

- `create_from_media_info()` — used by kithara-audio, untested
- `create_with_probe()` / `create_with_symphonia_probe()` — probe paths untested
- `TrackMetadata` struct — public type with zero coverage
- `InnerDecoder::update_byte_len()` — critical for HLS seeking, untested
- `InnerDecoder::reset()` — called after seeks, untested
- `InnerDecoder::metadata()` — metadata extraction untested

### 13. `kithara-stream`: Reader and Writer gaps

**Reader (partially tested via integration):**
- Basic `Read` implementation — empty buffer, `WaitOutcome` paths, error paths
- Segment-related methods: `current_segment_range()`,
  `format_change_segment_range()`, `clear_variant_fence()`

**Writer (tested via integration, gaps in error paths):**
- Empty chunk handling (warns and continues)
- Resource failure notification on error
- `WriterError` display messages

### 14. Cancellation behavior

No crate has dedicated tests for cancellation token behavior:
- Cancel during active download
- Cancel during on-demand fetch
- Cancel during backpressure wait
- Resource cleanup after cancellation
- Task handle abortion in Backend

---

## Recommendations

### Quick wins (high value, low effort)

1. **`EpochValidator` unit tests** — simple state machine, 5-6 tests
2. **`Stream::from_source()` + Read/Seek** — mock source, verify delegation
3. **`HlsConfig` / `FileConfig` builder tests** — verify defaults and chaining
4. **Error conversion tests** — verify all `From` implementations
5. **Event emission tests** — subscribe to channel, assert events in integration
   tests

### Structural improvements

1. **Extract testable logic from `apple.rs`** — separate offset estimation,
   packet buffering, and state management from FFI calls
2. **Add mock `Downloader` for `Backend` tests** — simple trait mock
3. **Add `KeyManager` unit tests** with known test vectors for AES-128
4. **Test `worker.rs` with mock decoder** — verify loop behavior, commands,
   backpressure

### Process improvements

1. **Run `cargo tarpaulin`** regularly to track line-level coverage
2. **Gate PRs on coverage for new code** — ensure new functions include tests
3. **Remove dead code** — `KeyError::KeyNotFound` is never used
