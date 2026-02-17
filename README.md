<div align="center">
  <img src="logo.svg" alt="kithara" width="400">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/zvuk/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/zvuk/kithara)
[![Crates.io](https://img.shields.io/crates/v/kithara.svg)](https://crates.io/crates/kithara)
[![Downloads](https://img.shields.io/crates/d/kithara.svg)](https://crates.io/crates/kithara)
[![docs.rs](https://docs.rs/kithara/badge.svg)](https://docs.rs/kithara)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

</div>

# kithara

> Built with AI, tested by a human. Vibe-coded -- but with care.
> Contributions, reviews, and fresh eyes are welcome.

> **Status: early development.** Public API will be kept as stable as possible, but internal architecture is actively being simplified — expect significant refactoring of larger crates. Pin to an exact version if you depend on kithara today.

Modular audio engine in Rust. Streams, decodes, and plays audio from progressive HTTP and HLS sources with persistent caching and offline support. Designed as an open-source alternative to AVPlayer with DJ-grade mixing capabilities — multi-slot playback, crossfading, BPM sync, and per-channel EQ.

Components are independent crates that can be used standalone or composed into a full player.

## Features

- **Player engine** — AVPlayer-style API with multi-slot arena, crossfading, BPM sync, and per-channel EQ (`kithara-play`)
- **Progressive file download** — stream MP3, AAC, FLAC and other formats over HTTP with disk caching and gap filling
- **HLS VOD** — adaptive bitrate streaming with variant switching, encrypted segments (AES-128-CBC), and offline support
- **Multi-backend decoding** — Symphonia (software, cross-platform) and Apple AudioToolbox (hardware, macOS/iOS)
- **Audio pipeline** — sample rate conversion via rubato, effects chain, OS-thread worker with backpressure
- **Persistent disk cache** — lease/pin semantics, LRU eviction, crash-safe writes
- **Zero-allocation hot paths** — sharded buffer pool for decode and I/O loops
- **WASM support** — browser playback via AudioWorklet with shared memory

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                      kithara                        │  facade
│                     kithara-play                    │  player engine
├──────────────────────┬──────────────────────────────┤
│    kithara-audio     │       kithara-events         │  pipeline
│    kithara-decode    │                              │
├──────────────────────┴──────────────────────────────┤
│  kithara-file    kithara-hls    kithara-abr         │  protocols
│                  kithara-drm                        │
├─────────────────────────────────────────────────────┤
│  kithara-stream         kithara-net                 │  I/O
├─────────────────────────────────────────────────────┤
│  kithara-assets         kithara-storage             │  storage
│  kithara-bufpool        kithara-platform            │  primitives
└─────────────────────────────────────────────────────┘
```

| Layer | Crates | Role |
|-------|--------|------|
| **Facade** | `kithara` | Unified `Resource` API with auto-detection (file / HLS) |
| **Player** | `kithara-play` | AVPlayer-style traits: Engine, Player, Mixer, DJ subsystem |
| **Pipeline** | `kithara-audio`, `kithara-decode`, `kithara-events` | Threaded decode + effects + resampling, event bus |
| **Protocols** | `kithara-file`, `kithara-hls`, `kithara-abr`, `kithara-drm` | HTTP progressive, HLS VOD with ABR, AES-128 decryption |
| **I/O** | `kithara-stream`, `kithara-net` | Async-to-sync bridge (`Read + Seek`), HTTP with retry |
| **Storage** | `kithara-assets`, `kithara-storage` | Disk cache with eviction, mmap/mem resources |
| **Primitives** | `kithara-bufpool`, `kithara-platform` | Zero-alloc buffer pool, cross-platform sync types |
| **Browser** | `kithara-wasm` | WASM player with AudioWorklet integration |

## Getting Started

```bash
# Build
cargo build --workspace

# Test
cargo test --workspace

# Lint
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
```

## Crate Documentation

Each crate has its own `README.md`:

- [`kithara`](crates/kithara/README.md) -- facade
- [`kithara-play`](crates/kithara-play/README.md) -- player engine
- [`kithara-audio`](crates/kithara-audio/README.md) -- audio pipeline
- [`kithara-decode`](crates/kithara-decode/README.md) -- Symphonia decoder
- [`kithara-stream`](crates/kithara-stream/README.md) -- async-to-sync bridge
- [`kithara-file`](crates/kithara-file/README.md) -- progressive file
- [`kithara-hls`](crates/kithara-hls/README.md) -- HLS VOD
- [`kithara-abr`](crates/kithara-abr/README.md) -- adaptive bitrate
- [`kithara-net`](crates/kithara-net/README.md) -- HTTP networking
- [`kithara-assets`](crates/kithara-assets/README.md) -- disk cache
- [`kithara-storage`](crates/kithara-storage/README.md) -- mmap storage
- [`kithara-bufpool`](crates/kithara-bufpool/README.md) -- buffer pool
- [`kithara-drm`](crates/kithara-drm/README.md) -- AES-128 decryption
- [`kithara-events`](crates/kithara-events/README.md) -- event bus
- [`kithara-platform`](crates/kithara-platform/README.md) -- platform primitives
- [`kithara-wasm`](crates/kithara-wasm/README.md) -- WASM player

## Examples

```bash
# Play a file with rodio
cargo run -p kithara --example resource_play --features rodio -- <URL_OR_PATH>

# Play a file (audio crate)
cargo run -p kithara-audio --example file_audio --features rodio,memprof -- <URL>

# Play HLS stream
cargo run -p kithara-audio --example hls_audio --features rodio,memprof -- <MASTER_PLAYLIST_URL>

# Play encrypted HLS
cargo run -p kithara-audio --example hls_drm_audio --features rodio,memprof -- <MASTER_PLAYLIST_URL>

# Analyze ABR switch behavior
cargo run -p kithara-decode --example abr_switch_simulator -- <HLS_DATA_DIR>
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, coding rules, and PR guidelines.

## Rules

See [`AGENTS.md`](AGENTS.md) for coding rules enforced across the workspace.

## Minimum Supported Rust Version (MSRV)

The current MSRV is **1.88** (Rust edition 2024). It is tested in CI and may be bumped in minor releases.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
