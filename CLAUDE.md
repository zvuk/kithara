# CLAUDE.md

@AGENTS.md
@.docs/workflow/rust-ai.md

## Claude Notes

- Project rules live in versioned repo files, not in auto memory.
- Path-scoped guidance lives in `.claude/rules/`; do not duplicate it here.

<!-- GSD:project-start source:PROJECT.md -->
## Project

**Kithara**

A modular audio streaming library in Rust — an open-source alternative to Apple AVPlayer with DJ-grade capabilities. Kithara streams, decodes, and plays audio from various sources (HLS, HTTP progressive, local files) with DRM support, adaptive bitrate, persistent disk cache, and multi-track playback. Target audience: streaming services and DJ software, with support for native (macOS/iOS/Android/Linux), WASM (browser), and FFI (Swift/Kotlin).

**Core Value:** Flawless audio quality — no sample should be lost, duplicated, or contain artifacts, whether during normal playback, seek, ABR switch, crossfade, or scratch.

### Constraints

- **License**: MIT OR Apache-2.0 — all dependencies must be compatible (no GPL)
- **Performance**: zero-allocation in hot paths (audio pipeline, read_at, seek). Lock-free ring buffers for PCM
- **WASM compatibility**: all shared abstractions must work on wasm32-unknown-unknown
- **No unsafe in most crates**: `#![forbid(unsafe_code)]` except kithara-platform
- **Workspace-first deps**: versions only in root Cargo.toml `[workspace.dependencies]`
- **No unwrap/expect**: `clippy.toml` enforces `unwrap_used = "deny"` in production code
- **Rust Edition 2024**: MSRV maintained, nightly only for WASM/bench
<!-- GSD:project-end -->

<!-- GSD:stack-start source:codebase/STACK.md -->
## Technology Stack

## Languages
- Rust 1.89 - Audio streaming/playback library with HLS, file decoding, DRM support
- Edition: 2024 (latest)
- Swift - Apple/iOS FFI bindings (see `apple/` directory)
- Kotlin - Android JNI bindings (see `android/` directory)
- TypeScript/JavaScript - WASM integration and browser-based player
## Runtime
- Rust Standard Library with custom `kithara-platform` abstraction layer
- Tokio 1.50.0 - Async runtime for network I/O and background tasks
- `tokio_with_wasm` 0.9 - Cross-platform async (native + wasm32)
- `x86_64-unknown-linux-gnu` - Linux
- `aarch64-apple-darwin` / `x86_64-apple-darwin` - macOS
- `aarch64-apple-ios` / `armv7-apple-ios` - iOS (via FFI/Kotlin)
- `aarch64-linux-android` - Android (via JNI)
- `wasm32-unknown-unknown` - WebAssembly (browser audio)
- Cargo (Rust package manager)
- Workspace resolver: 2
- Lockfile: `Cargo.lock` (committed)
- Workspace-first dependency management: All versions declared in root `Cargo.toml` `[workspace.dependencies]`, crates reference via `{ workspace = true }`
## Frameworks
- Symphonia 0.6.0-alpha.1 - Synchronous audio codec decoding (AAC, MP3, FLAC, etc.)
- Firewheel 0.10.0 - Modular audio graph/DSP engine with scheduling
- Rodio 0.22.2 - Audio output abstraction (optional fallback)
- Bevy Platform 0.18 - Web Audio API bindings for WASM
- Reqwest 0.13.2 - HTTP client with rustls, streaming support
- Tokio-util 0.7.18 - Async I/O utilities (stream codecs, buffer management)
- HLS_M3U8 0.5.1 - HLS playlist parsing
- Iced 0.14 - Cross-platform GUI (Elm-inspired)
- Iced_aw 0.13.1 - Iced widgets addon
- Ratatui 0.30.0 - Terminal UI framework
- Crossterm 0.29 - Cross-platform terminal control
- Rstest 0.26.1 - Parametrized tests with async/timeout support
- Unimock 0.6 - Zero-overhead trait mocking and partial type builders
- Serial_test 3.4.0 - Serialize concurrent test execution (for resource contention tests)
- Wasm-bindgen-test 0.3 - WASM test runner
- Just 1.x - Command runner (taskfile alternative)
- Cargo xtask - Workspace orchestration
- Criterion 0.8.2 - Benchmarking framework
- Nextest - Faster test runner replacement for cargo test
- Rustfmt (nightly) - Code formatting
- Clippy - Rust linter with strict workspace settings
- Cargo-deny - Dependency security/license audit
- Cargo-machete - Unused dependency detector
- Cargo-semver-checks - Semver compatibility validation
- Ast-grep - Custom linting rules (style, architecture)
- Tracing 0.1.44 - Structured logging/telemetry (preferred over println!)
- Thiserror 2.0.18 - Error type derivation
- Cargo-llvm-cov - Code coverage measurement
- Hotpath 0.14.0 - Zero-overhead timing (timing-only mode in dev, allocation in release)
- Memory-stats 1.2 - Memory reporting
- Stats_alloc 0.1 - Allocation tracking for zero-alloc hot path validation
- Assert_no_alloc 1.1 - Compile-time zero-alloc verification
- Dhat 0.3 - Memory profiling integration
## Key Dependencies
- Tokio 1.50.0 - Async runtime for network, background playback thread
- Symphonia 0.6.0-alpha.1 - Decoding engine (AAC, MP3, FLAC, Opus, Vorbis)
- Firewheel 0.10.0 - Audio graph/DSP (nodes, scheduling, transport)
- Reqwest 0.13.2 - HTTP downloads with retry/timeout semantics
- Bytes 1.11.1 - Efficient byte handling
- Ringbuf 0.4.8 - Lock-free ringbuffer (audio pipelines)
- Slice_ring_buf 0.3 - Alternative ringbuffer variant
- Rangemap 1.7.1 - Range-based storage indexing (with serde1 feature)
- Mmap-io 0.9 - Memory-mapped file I/O (native only, not wasm32)
- Dashmap 6.1.0 - Concurrent hashmap for shared state
- LRU 0.16 - LRU eviction for asset caching
- Tempfile 3.27.0 - Crash-safe temp file writing
- Dasp 0.11 - Audio signal processing (resampling, interpolation)
- Rubato 1.0 - High-quality audio resampling (sinc interpolation)
- Audioadapter-buffers 2.0 - Channel buffer format translation
- Biquad 0.5.0 - Biquad filter DSP (EQ)
- Fast-interleave 0.1 - Optimized channel interleave/deinterleave
- Portable-atomic 1.13.1 - Atomic operations across platforms
- URL 2.5.8 - URL parsing (HLS segment URLs, resource keys)
- Axum 0.8.8 - Web framework for local test servers
- Wasm_safe_thread 0.1.1 - Thread compatibility layer (native + wasm32)
- Parking_lot 0.12 - Faster synchronization primitives (RwLock, Mutex)
- Web-time 1.1 - `std::time` compatibility for wasm32
- Gloo-timers 0.4 - Browser timer API (`setTimeout`, `setInterval`)
- Serde 1.0 - Serialization framework (derived)
- Serde_json 1.0 - JSON (test fixtures, logging)
- Postcard 1 - Compact binary serialization (crate-internal)
- AES 0.8.4 - AES encryption (DRM key block)
- CBC 0.1.2 - CBC cipher mode (HLS DRM standard)
- SHA2 0.11 - SHA-256 hashing
- Hex 0.4 - Hex encoding/decoding (DRM keys)
- Rand 0.10.0 - Random number generation
- Rustls-platform-verifier 0.6.2 - TLS cert verification (cross-platform)
- Obfstr 0.4.4 - String obfuscation (test URLs)
- Crabtime 1.1.4 - Time obfuscation (test data)
- Uniffi 0.31.0 - Kotlin/Swift FFI scaffold generator
- Wasm-bindgen 0.2 - WASM/JS interop
- Js-sys 0.3 - JavaScript APIs
- Web-sys 0.3 - Web APIs (AudioContext, AudioWorklet, File, etc.)
- Thirtyfour 0.36.1 - Selenium WebDriver (integration tests, browser automation)
- Delegate 0.13 - Trait delegation macros
- Bitflags 2.11 - Bitfield enum generation
- Derivative 2.2 - Derive helpers (non-exhaustive)
- Derive_setters 0.1 - Builder pattern generation
- Smallvec 1 - Stack-allocated vectors (small defaults)
- Futures 0.3.32 - Async/await primitives
- Fallible-iterator 0.3 - Iterators with fallible `next()`
- Tower 0.5.3 - Middleware/service abstraction
- Tower-http 0.6 - HTTP middleware (CORS, static files, headers)
- UUID 1 - UUID generation (asset IDs)
- Proc-macro2, quote, syn 2.x - Proc macro tools
- Console_error_panic_hook 0.1 - Panic hook for WASM debugging
- Cargo_metadata 0.23 - Cargo.toml parsing (xtask)
- Anyhow 1 - Context-rich error propagation (xtask)
- Regex 1 - Pattern matching (xtask filtering)
## Configuration
- No `.env` file usage (configuration via command-line or in-code defaults)
- Environment variables for CI:
- Workspace-level profiles:
- Features: `file`, `hls`, `assets`, `net`, `bufpool`, `drm`, `test-utils`, `perf`, `apple`, `disable-hang-detector`
- Target-specific:
## Platform Requirements
- Rust 1.89+ (edition 2024)
- Nightly Rust (for rustfmt, some test features)
- Just (command runner)
- pkg-config, libasound2-dev (Linux audio)
- chromedriver/Selenium (for wasm/browser tests)
- Xcode (macOS/iOS builds)
- Android SDK + Gradle (Android builds)
- Optional: cargo-deny, cargo-machete, cargo-semver-checks, cargo-hack, ast-grep, critcmp (benchmarks), wasm-slim (WASM size checks)
- **Deployment target:** Library (not standalone binary)
- **Memory:** Minimal (ringbuffers, LRU cache are bounded)
- **Network:** HTTP/HTTPS (TLS via rustls-platform-verifier)
- **Audio output:** Platform-specific
- **Caching:** Disk (mmap via kithara-storage) or in-memory (optional)
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

## Naming Patterns
- Module files mirror their logical content: `downloader.rs`, `fetch.rs`, `stream.rs`, `error.rs`
- Test files in `/tests/tests/` follow pattern `{feature}_{test_case}.rs` (e.g., `sync_reader_basic_test.rs`, `driver_test.rs`)
- Proc-macro crates use `src/lib.rs` for the macro definition
- Substantial test fixtures live in `/tests/` directory (not in `src/`)
- Constructor: `new()` (e.g., `Stream::new()`, `HlsConfig::new()`)
- Builder pattern: `with_{field}()` for chainable setters (e.g., `with_abr_options()`, `with_store()`, `with_events()`)
- Getters: simple field names without prefix (e.g., `position()`, `timeline()`, `source()`)
- Query/check methods: `is_{condition}()` (e.g., `is_empty()`, `is_retryable()`)
- Async operations: `async fn {action}()` (e.g., `Stream::new()` is async)
- Local bindings use snake_case throughout
- Loop indices: `i`, `variant_index`, `segment_index` (descriptive over single letters when meaningful)
- Mutable state: clearly named (e.g., `source`, `last_error`, `cloned`)
- Error handling: `error`, `err` for generic errors; domain-specific for typed errors
- Enums: PascalCase variants (e.g., `NetError::Http`, `NetError::Timeout`, `SourcePhase::Loading`)
- Named-field structs (public across crate boundaries): `#[non_exhaustive]` attribute mandatory (e.g., `MediaInfo`, `HlsError`)
- Trait implementations: generic over parameters with clear type names (e.g., `Stream<T: StreamType>`)
- Error types: use `{Feature}Error` convention (e.g., `NetError`, `HlsError`, `StreamError<E>`)
- Result type aliases: `{Feature}Result<T>` (e.g., `NetResult<T>`, `HlsResult<T>`, `StreamResult<T, E>`)
## Code Style
- Tool: `rustfmt` with `rustfmt.toml` at workspace root
- Max width: 100 characters
- Line style: Unix (`\n` only)
- Imports: grouped and sorted (`StdExternalCrate` order)
- Tab spaces: 4
- Use field init shorthand: `Field { x, y }` instead of `Field { x: x, y: y }`
- Try shorthand: use `?` operator instead of `match` on `Result`
- Tool: `clippy` via `clippy.toml` with workspace lint settings (`Cargo.toml` `[workspace.lints]`)
- `unwrap_used = "deny"` in production code; allowed only in tests (`allow-unwrap-in-tests = true`)
- No unsafe code: `#![forbid(unsafe_code)]` required in most crates
- `unreachable_pub = "warn"`: do not expose internal helpers with bare `pub`
- `doc_markdown = "warn"`: proper rustdoc formatting
- Cognitive complexity threshold: 35 (see `clippy.toml`)
- Too many lines threshold: 150 per file (architectural signal to split)
- Keep in-code comments SHORT and LOCAL to explain WHY, not WHAT
- No separator comments (`// ====`), banner comments, or ad-hoc style variants
- Contracts, invariants, lifecycle, protocol, or cache explanations belong in the owning crate `README.md`
- Rustdoc on public items (functions, types) is mandatory; include error cases for `#[must_use]`
- Module-level doc comments: use `//!` to describe the module's purpose
- Example in rustdoc: use `ignore` directive if example requires setup: `` ```ignore ``
## Import Organization
- Use workspace member shortcuts when available (e.g., `use kithara_stream::Stream`)
- Prefer short readable names in the body over repeated deep qualified paths
- Full paths acceptable only when they resolve name conflicts
- Keep `use` imports at the TOP of the file; NEVER place `use` inside functions, methods, or blocks
- Use `imports_granularity = "Crate"` (per `rustfmt.toml`): consolidate imports to crate level
## Visibility Model
- Default: `pub(crate)` for internal helpers and implementation details
- Promote to `pub` only when:
- ALL public structs and enums with named fields exposed across crate boundaries: `#[non_exhaustive]`
- Example: `pub struct MediaInfo` in `kithara-stream`, `pub enum NetError` in `kithara-net`, `pub enum HlsError` in `kithara-hls`
- Small, obviously stable exceptions: OK only when extension is unlikely AND direct construction is part of intended contract
## Error Handling
- Use typed errors with `thiserror::Error` (mandatory for public APIs)
- Error variants carry context (URL, status code, resource name) — see `NetError::HttpError { status, url, body }`, `HlsError::HttpError { ... }`
- Generic error parameters: `StreamError<E>` where `E: StdError + Send + Sync + 'static`
- No `unwrap()` or `expect()` in production code without strong, explicit reason
- Use `?` operator for error propagation; chains are readable and explicit
- `#[error("...")]` derives `Display` via `thiserror`
- Include failed resource context: `#[error("HTTP {status}: {body:?} for URL: {url:?}")]`
- Log errors with `tracing` fields: `asset_id`, `url`, `resource`, `variant`, `status`, `attempt`, `timeout_ms`
- Never log secrets (API keys, auth tokens, etc.)
## Logging
- Spans for async/request context: `#[tracing::instrument(skip(...))]` on functions
- Structured fields: `debug!(segment_idx = ?, bytes = ?, "loading segment")`
- Levels: `trace!()` for detailed debug output, `debug!()` for operational events, `warn!()` for recoverable issues
- Entry point: See `HlsEvent`, `Event` hierarchies in `kithara-events`
- `kithara-hls/downloader.rs`: `debug!("downloading segment...")`, `trace!("segment loaded")`
- `kithara-net/retry.rs`: no logging (handled at call site)
- Field context: `segment_idx`, `bytes`, `attempt`, `timeout_ms`, `url`, `resource`, `variant`
## Function Design
- Prefer small, focused functions (under 50 lines is common)
- Extract distinct subsystems into their own files (see `kithara-hls`: `downloader.rs`, `fetch.rs`, `source.rs`, `stream_index.rs`)
- Very large impl blocks signal refactoring opportunity
- Avoid many parameters: use builder pattern for optional config (e.g., `HlsConfig::new(url).with_store(...).with_events(...)`)
- Generic type parameters: prefer when extending behavior (e.g., `Stream<T: StreamType>`, `RetryNet<N: Net, P: RetryPolicyTrait>`)
- Closures for composition: `DecoderFactory<T>` = `Arc<dyn Fn(SharedStream, MediaInfo, offset) -> Box<dyn Decoder>>`
- Async constructors return `Result<Self, E>`: `async fn new(config) -> Result<Self, Error>`
- Builder methods return `Self` for chaining
- Fallible operations return `Result<T, E>` with typed error
- Query methods return `Option<T>` when result may not exist (e.g., `len()`, `current_segment_range()`)
- Sync constructors when possible: `StreamType::create()` is synchronous, spawns internal tasks
- Async only for I/O-blocking operations
- Use `#[async_trait]` with `#[cfg_attr]` for WASM+native compat (see `kithara-net/retry.rs`)
## Module Design
- `lib.rs` and `mod.rs` contain ONLY module declarations (`mod foo;`) and re-exports (`pub use ...`)
- Concrete types/logic live in sibling `.rs` files or submodules
- Example: `kithara-stream/lib.rs` re-exports `Stream`, `StreamType`, `Backend`, etc. from their own files
- `lib.rs` re-exports public API: `pub use crate::{stream::Stream, backend::Backend, ...}`
- Avoid exporting internal types; use `#[doc(hidden)]` if re-export is necessary but unstable
- Use `#[cfg(feature = "...")]` to enable protocol-specific internals
- Example: `#[cfg(feature = "internal")] pub mod internal;` exposes internals for integration tests
- Examples: `kithara-net`, `kithara-hls`, `kithara-stream` all have `internal` feature for test helpers
## Shared Types and Canonicity
- `AudioCodec`, `ContainerFormat`, `MediaInfo` live ONLY in `kithara-stream`
- Do NOT duplicate these across HLS, File, or other protocol crates
- Prefer generics and composition over protocol-specific type copies
- `type HlsResult<T> = Result<T, HlsError>` — encourages consistent error handling
- `type NetResult<T> = Result<T, NetError>`
- `type StreamResult<T, E> = Result<T, StreamError<E>>`
## Workspace-First Dependencies
- ALL dependency versions declared in root `Cargo.toml` under `[workspace.dependencies]`
- Crates reference with `{ workspace = true }`
- No per-crate version overrides unless there is a strong, documented reason
- Justification for exceptions goes in the task/plan/PR description and crate `README.md`
## Proc Macros
- Attribute macros for test decoration: `#[kithara::test(...)]` (see `kithara-test-macros`)
- Derive macros for boilerplate (setters, builder): `#[derive(Setters)]` (see `MediaInfo`)
- Proc macros live in separate crates: `kithara-test-macros`, `kithara-hang-detector-macros`, `kithara-wasm-macros`
## Code Generation and Fixtures
- Large fixtures, local servers, generated content → `/tests/` directory
- Small unit test helpers under `#[cfg(test)]` next to code is fine
- Example: `kithara-test-utils/` contains `signal_pcm.rs`, `wav.rs`, `asset_server.rs` for test infrastructure
- Example: `tests/tests/` contains integration tests with full test data (HLS variants, DRM-encrypted segments)
<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

## Pattern Overview
- Async network/IO layer produces data; sync codec layer consumes it via shared storage
- Generic `Stream<T>` parameterized over protocol types (File, Hls, etc.)
- Each protocol implements the `StreamType` trait to plug into the generic framework
- Multi-threaded architecture with clear separation of concerns: downloader async task, decoder worker OS thread, reader sync thread
- Feature-gated optional modules: HLS, file, ABR, DRM, app frontends (TUI/GUI)
## Layers
- Purpose: Provides the async-to-sync bridge and generic stream orchestration framework
- Location: `crates/kithara-stream/src/`
- Contains: `StreamType` trait, `Stream<T>` generic type, `Source` sync trait, `Downloader` async trait, `Backend` worker orchestration, `Timeline`, `Topology`, `LayoutIndex` layout abstractions
- Depends on: `kithara-platform` (sync primitives), `kithara-storage` (StorageResource), `kithara-net` (HTTP client trait)
- Used by: All stream implementations (HLS, File), audio pipeline, decoders
- Purpose: Implement `StreamType` trait for specific protocols; handle protocol-specific fetching, parsing, variant switching
- Location: `crates/kithara-hls/src/`, `crates/kithara-file/src/`
- Contains: 
- Depends on: `kithara-stream`, `kithara-abr` (HLS only), `kithara-drm` (HLS only), `kithara-assets` (caching), `kithara-net` (HTTP)
- Used by: Decoding pipeline, applications
- Purpose: Convert compressed audio bytes to PCM samples; apply effects and resampling
- Location: `crates/kithara-decode/src/`, `crates/kithara-audio/src/`
- Contains:
- Depends on: `kithara-stream` (MediaInfo, Stream types), `symphonia` (decoder), `rubato` (resampler)
- Used by: Playback layer, applications
- Purpose: Type-erased unified interface for audio resources; auto-detection of stream type
- Location: `crates/kithara-play/src/`
- Contains: `Resource` (type-erased wrapper), `SourceType` auto-detection (HLS vs File), `PlayerImpl`, `EngineImpl` (audio engines)
- Depends on: `kithara-audio`, `kithara-hls`, `kithara-file`, `kithara-decode`
- Used by: Applications, WASM bindings
- **kithara-events**: Hierarchical event bus with feature-gated event types (FileEvent, HlsEvent, AudioEvent, PlayerEvent, AppEvent)
- **kithara-platform**: Native/WASM-compatible sync primitives (Mutex, RwLock, Condvar), thread abstractions
- **kithara-net**: HTTP client trait and implementations with retry/timeout logic
- **kithara-storage**: Mmap-backed and in-memory resource drivers for buffering
- **kithara-assets**: Persistent disk cache with lease-based pinning and DRM decryption
- **kithara-bufpool**: Pool-backed byte buffers for allocation amortization
- **kithara-abr**: Protocol-agnostic adaptive bitrate controller (throughput estimation + variant selection)
- **kithara-drm**: AES-128-CBC decryption for encrypted segments
- **kithara-app**: Unified app crate with feature-gated TUI (`tui` feature, ratatui-based) and GUI (`gui` feature, iced-based) frontends
- **kithara-wasm**: WASM bindings for browser deployment; worker-threaded architecture
- **kithara-ffi**: FFI bindings (uniffi, JNI) for mobile (iOS/Android)
## Data Flow
- **StorageResource** (shared `Mutex<RangeSet>` + `Condvar`): Tracks which byte ranges are available; downloader writes updates, reader waits on condvar
- **DownloadState / SharedSegments** (HLS-specific): Virtual stream layout mapping segments to byte offsets; updated on variant switch
- **Progress** (AtomicU64): Current reader position; used for backpressure calculation
- **Playlist** (HLS, `RwLock<Option<PlaylistState>>`): Shared between handler and bytestream; lazy-loaded
- **Timeline** (shared `Arc<RwLock<Timeline>>`): Playback time tracking
## Key Abstractions
- Purpose: Marker trait defining protocol-specific creation and event handling
- Examples: `impl StreamType for Hls` in `crates/kithara-hls/src/inner.rs`, `impl StreamType for File` in `crates/kithara-file/src/inner.rs`
- Pattern: Associated types for Config, Topology, Layout, Coord, Demand, Source, Error, Events; `create()` async factory method
- Purpose: Sync random-access interface for reading data at arbitrary offsets
- Location: `crates/kithara-stream/src/source.rs`
- Methods: `wait_range(pos, len) -> WaitOutcome` (blocks until ready or cancelled), `read_at(pos, buf) -> ReadOutcome` (sync read), `len()`, `media_info()`, `phase()`
- Returns `SourcePhase` (Waiting, Ready, Seeking, Eof, Cancelled, etc.) for external observers
- Purpose: Async planner for download work; orchestration logic per protocol
- Location: `crates/kithara-stream/src/downloader.rs`
- Methods: `plan() -> PlanOutcome` (returns batches of work), `commit(results)` (stores outcomes), `should_throttle()` (backpressure check)
- Protocol-specific examples: `HlsDownloader` (playlist refresh, segment selection, key fetching), `FileDownloader` (linear range fetching)
- Purpose: Generic orchestration loop spawning and managing the downloader task
- Location: `crates/kithara-stream/src/backend.rs`
- Spawns on: Explicit tokio handle (if provided) → current multi-thread runtime → dedicated thread with own current-thread runtime (fallback)
- Manages: Cancellation token, worker handle (thread or task), downloader loop lifecycle
- Purpose: Wraps `Source` providing sync `Read + Seek` interface
- Location: `crates/kithara-stream/src/stream.rs`
- Implements: `std::io::Read`, `std::io::Seek`
- Pattern: Calls `wait_range()` before reading; polls source phase to detect variant changes or EOF
- Purpose: Implements `Source` for HLS; manages virtual stream layout across variants
- Location: `crates/kithara-hls/src/source.rs`
- Maintains: `DownloadState` (segment-to-byte mapping), playlist state, variant switching logic with `VariantFence` barriers
- Coordinates with: `HlsDownloader` via shared `StreamIndex` and `SharedPlaylist`
- Purpose: Runtime codec selection and decoder instantiation
- Location: `crates/kithara-decode/src/`
- Pattern: `DecoderFactory::create()` returns `Box<dyn InnerDecoder>` (Symphonia by default, Apple AudioToolbox on macOS/iOS)
- Usage in `Audio`: Closure passed to `Audio` for creating decoder from `SharedStream<T>` + `MediaInfo`
- Purpose: Threaded decode, effects, resampling orchestration
- Location: `crates/kithara-audio/src/`
- Thread model: Dedicated worker thread using round-robin cooperative scheduling (multiple tracks share one worker)
- Channels: PCM chunks via lock-free `ringbuf`, commands via lock-free `ringbuf`, events via `EventBus`
## Entry Points
- Location: `crates/kithara-stream/src/stream.rs`, `Stream::<T>::new(config)`
- Triggers: Application calls `Stream::<Hls>::new(config)` or `Stream::<File>::new(config)`
- Responsibilities: 
- Location: `crates/kithara-hls/src/inner.rs`, `impl StreamType for Hls`
- Triggers: `Stream::<Hls>::new(HlsConfig)`
- Responsibilities:
- Location: `crates/kithara-file/src/inner.rs`, `impl StreamType for File`
- Triggers: `Stream::<File>::new(FileConfig)`
- Responsibilities:
- Location: `crates/kithara-audio/src/audio.rs`, `Audio::<S>::new(config)`
- Triggers: Application calls `Audio::<Stream<T>>::new(config)` or `Audio::<File>::new(config)`
- Responsibilities:
- Location: `crates/kithara-play/src/impls/resource.rs`, `Resource::new(config)`
- Triggers: Application with URL (HLS auto-detected by `.m3u8` suffix)
- Responsibilities:
## Error Handling
- **Stream Layer**: `StreamError` with variants (ConfigError, NetworkError, FormatError, StorageError, Cancelled)
- **HLS Layer**: `HlsError` wrapping download errors, parsing errors, variant change errors, encryption errors
- **File Layer**: `FileError` for configuration and download failures
- **Decode Layer**: `DecodeError` for format probing, codec initialization, decoding failures
- **Audio Layer**: `DecodeError` surfaced; errors logged and optionally published via `EventBus`
- **Storage Layer**: `StorageError` for eviction, truncation, permission issues
- **Network Layer**: `NetError` with retry policy tracking, timeout, connection errors
- Network: Automatic retry with exponential backoff (configurable)
- Eviction: Caller retries from `wait_range()` (Retry outcome)
- Variant Change: Decoder recreation barrier at fence; caller must acknowledge
- Seek Failures: Logged; playback may stall or resume from nearest valid position
## Cross-Cutting Concerns
- Framework: `tracing` crate with structured fields
- Usage: `debug!()`, `info!()`, `warn!()`, `error!()` macros with contextual fields (asset_id, url, variant_index, segment_idx, bytes, attempt, timeout_ms)
- No `println!()` or `dbg!()` in production code per AGENTS.md
- Configuration: Validated at creation time (URL parsing, DRM key setup)
- Byte ranges: Validated by `StorageResource` (no overlaps, no out-of-bounds reads)
- Variant switches: Gated by `SourcePhase::Seeking` and `VariantFence` barriers
- Inputs: Seek positions validated against stream length; out-of-bounds seeks return EOF
- Encryption: AES-128-CBC decryption via `kithara-drm` (keyed per segment, processor callback in assets)
- Key fetching: Coordinated by HLS downloader; optional custom `KeyProcessor` for in-house DRM
- Headers: Custom headers passed through `kithara-net` client
- Mechanism: `tokio::sync::CancellationToken` propagated to downloader; dropping `Backend` cancels child token
- Propagation: Downloader checks token before each fetch; reader sees `SourcePhase::Cancelled`
- Effects: In-flight fetches may be interrupted; storage updates are synchronously finalized before exit
- Mechanism: `should_throttle()` checked by downloader; pauses when `download_pos - read_pos > look_ahead_bytes`
- On-Demand: Variant switch requests bypass backpressure; initiated via `TransferCoordination` trait
- Reader Position: Updated by decoder via `Progress` atomic (RwLock wait-free reads in downloader loop)
<!-- GSD:architecture-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd:quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd:debug` for investigation and bug fixing
- `/gsd:execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->

<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd:profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
