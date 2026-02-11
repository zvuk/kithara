# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-02-11

### Added

- Initial release of the kithara audio streaming library.
- 12 modular crates: kithara, kithara-audio, kithara-decode, kithara-stream, kithara-file, kithara-hls, kithara-abr, kithara-net, kithara-assets, kithara-storage, kithara-bufpool, kithara-drm.
- Progressive file download with disk caching and gap filling.
- HLS VOD streaming with adaptive bitrate switching.
- AES-128-CBC segment decryption for encrypted HLS streams.
- Multi-backend audio decoding (Symphonia software, Apple AudioToolbox hardware).
- Sample rate conversion via rubato with five quality levels.
- Persistent disk cache with lease/pin semantics and LRU eviction.
- Zero-allocation buffer pools for hot paths.
- Async-to-sync bridge for streaming data to synchronous decoders.
- Event-driven architecture with broadcast channels.

[Unreleased]: https://github.com/zvuk/kithara/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/zvuk/kithara/releases/tag/v0.1.0
