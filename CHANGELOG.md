# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.0.1-alpha1] - 2026-05-19

First public alpha. Pre-release: public APIs may shift between alpha tags.

### Added

- Player engine (`kithara-play`): AVPlayer-style API on top of Firewheel audio graph with multi-slot arena, crossfading, BPM sync, per-channel EQ, and DJ-ready architecture.
- Queue layer (`kithara-queue`): AVQueuePlayer-analogue with queue, loader, navigation, and crossfade-aware track selection.
- Adaptive bitrate (`kithara-abr`): protocol-agnostic ABR with pull-driven decisions, manual switch, and seed-based initial throughput estimation.
- HLS VOD (`kithara-hls`): variant switching, cross-codec recreate, AES-128-CBC decryption, container-aware ABR commit (fMP4 same-codec recreate), HlsReaderHooks for `ReaderSeek` + `SegmentReadStart`.
- Progressive file (`kithara-file`): gap-driven `FilePeer`, pull-driven loop, MP3/AAC/FLAC streaming over HTTP.
- Decode (`kithara-decode`): `UniversalDecoder<D, C>` orchestrator over Demuxer + FrameCodec; Apple AudioToolbox native standalone (WAV/MP3/ALAC) via AudioFileServices; Android `MediaExtractor` standalone; Symphonia software backend; CBR batching with preserved `io::Error` chain; `Frame.packet_desc` for VBR.
- DRM (`kithara-drm`): AES-128-CBC end-to-end with extensible `KeyProcessor`.
- Events (`kithara-events`): unified `EventBus` with hierarchical `BusScope`, feature-gated by surface (`file`/`hls`/`audio`/`player`).
- App (`kithara-app`): unified app crate with feature-gated `tui` (ratatui) and `gui` (iced) frontends; single binary `kithara` with `--mode auto|tui|gui`.
- FFI (`kithara-ffi`): cross-platform FFI adapter for the player; UniFFI bindings for Apple (Swift) and Android (Kotlin).
- WASM (`kithara-wasm`): browser playback bindings with shared-memory threading, Trunk-served demo, `wasm_safe_thread` runtime.
- Workspace tooling (`xtask`): three lint namespaces (`arch` 33 checks, `style` 5, `idioms` 19) with comment-preserving autofix engine; ast-grep policy filter (55 rules); workspace typos wrapper; health reporter; publish-order resolver; orphans-per-package; similarity audit; arch visualisation.
- USDT probe macro `#[kithara::probe(...)]` for runtime tracing across HLS/stream/ABR/audio/decode.
- E2E suite (`tests/`) with `suite_light` / `suite_heavy` / `suite_stress` targets, nextest stress mode, Selenium WASM scenarios.

### Changed

- Workspace-wide migration to `bon::Builder` for all `*Config` structs (`PlayerConfig`, `ResourceConfig`, `DownloaderConfig`, `NetOptions`, `StoreOptions`, `FetchCmd`, `KeyProcessorRule`, `MediaInfo`, `SourceSeekAnchor`, …).
- Unified transport: `kithara_stream::dl::Downloader` is the global HTTP pool; `PeerHandle` is the per-track API; `Downloader::register(peer)` registers `Peer` implementations.
- HLS rewrite per Plans 05+06: `HlsCoord` + `HlsTrack` with persistent queue, cancel-token epoch, interior mutability for production wiring.
- `kithara-stream`: non-`Optional` `SegmentLayout::init_segment_range`; `Stream::Read` retry signal — `wait_range` errors must contain `"budget exceeded"` to be treated as transient.
- `kithara-test-utils` split into `hang` / `mock` / `probe` / `test` submodules with granular cargo features; `hang-detector` crate merged into `test-utils::hang`.
- `kithara-test-macros` decomposed into `test` and `probe` submodules.
- Integration test domain moved out of `kithara-test-utils` into `kithara-integration-tests` (`tests/` crate); tests now drive via public API rather than white-box helpers.
- Tests consolidated via `#[case]` parametrize; duplicate suite_heavy mounts dropped.
- Public types across crate boundaries marked `#[non_exhaustive]`.

### Fixed

- DRM PKCS7 size shrink: refresh segment range mid-fill so demuxer absorbs the shrink.
- Manual ABR switch fires immediately; cross-codec recreate stability under switch+seek.
- One-shot fMP4 parse + seek-collapse cross-variant continuity.
- Cross-variant byte continuity on `Auto` switch.
- Reentrant deadlock in `poll_state_phase` via `AbrController` tick.
- `Source::current_segment_range`, `media_info`, `format_change_segment_range` correctness after HlsCoord rewrite and ABR commit.
- Audio worker wake-up after `clear_seek_pending` so ABR lock releases when peer is idle.
- WAV continuity race, cross-codec init, segment-index propagation.
- Size-based recreate-readiness gate for backwards seek after manual variant switch.

### Removed

- `kithara-hang-detector` crate (merged into `kithara-test-utils::hang`).
- `kithara-ui` and `kithara-tui` crates (collapsed into `kithara-app::gui` / `kithara-app::tui` modules behind feature flags).
- `DownloadState` (replaced by `StreamIndex`).
- `HlsSegmentView`, demand machinery, `ResourceConfig` chain shims, `commit_variant_layout`.
- White-box `test_helpers`; tests now use only the public API.

[Unreleased]: https://github.com/zvuk/kithara/compare/v0.0.1-alpha1...HEAD
[0.0.1-alpha1]: https://github.com/zvuk/kithara/releases/tag/v0.0.1-alpha1
