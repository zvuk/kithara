<div align="center">
  <img src="logo.svg" alt="kithara" width="400">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara.svg)](https://crates.io/crates/kithara)
[![docs.rs](https://docs.rs/kithara/badge.svg)](https://docs.rs/kithara)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

</div>

# kithara

> Open-source modular audio engine in Rust.
> Contributions and reviews are welcome.

> **Status: active development.** Public APIs are intended to remain stable within a release line, while internal implementation may evolve. Pin exact versions for production use.

Modular audio engine in Rust. Streams, decodes, and plays audio from
progressive HTTP and HLS sources with a persistent on-disk cache. Designed as
an open-source alternative to AVPlayer with DJ-grade mixing — multi-slot
playback, crossfading, and per-channel EQ.

Components are independent crates: use one standalone, or compose them into a
full player.

## Features

- **Player engine** — AVPlayer-style API with multi-slot playback, crossfading, and per-channel EQ (`kithara-play`)
- **Progressive HTTP** — stream MP3, AAC, FLAC, and ALAC with disk caching and pull-driven range fetches
- **HLS VOD** — adaptive bitrate, variant switching, cross-codec recreate, AES-128-CBC encrypted segments, on-disk segment cache
- **Multi-backend decode** — Symphonia (software, cross-platform), Apple AudioToolbox (macOS/iOS), Android `MediaCodec` (Android)
- **Audio pipeline** — fixed-ratio sample-rate conversion, effects chain, OS-thread worker with backpressure
- **Persistent disk cache** — lease/pin semantics, LRU eviction, crash-safe writes
- **Zero-allocation hot paths** — sharded buffer pool for decode and I/O loops
- **WASM** — browser playback bindings with shared-memory threading (`kithara-ffi` web module)

## Quick Start

```bash
cargo install just --locked
cargo build --workspace
just test-all
```

Full environment setup, lint commands, and mobile (Android / Apple) build flows
live in [CONTRIBUTING.md](CONTRIBUTING.md).

## Demo Players

```bash
# Native demo — auto-picks TUI or GUI for the terminal
cargo run -p kithara-app -- --mode auto <TRACK_URL_1> <TRACK_URL_2>
cargo run -p kithara-app -- --mode tui <TRACK_URL_1> <TRACK_URL_2>
cargo run -p kithara-app -- --mode gui <TRACK_URL_1> <TRACK_URL_2>

# WASM browser demo (via kithara-ffi)
cd crates/kithara-ffi
RUSTUP_TOOLCHAIN=nightly trunk serve --config Trunk.toml --port 8080
```

## Architecture

A layered workspace of independent crates, from the public player API down to
storage and platform primitives. See [ARCHITECTURE.md](ARCHITECTURE.md) for the
facade architecture and [CONTEXT.md](CONTEXT.md) for the crate map, data flow,
and cross-crate contracts. Each crate also has its own `README.md`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and
[docs/workflows/rust-ai.md](docs/workflows/rust-ai.md) for the local-first task
flow.

## Minimum Supported Rust Version (MSRV)

The current MSRV is **1.89** (Rust edition 2024), tracked via `rust-version` in
the workspace `Cargo.toml`. It may be bumped in pre-release alphas without a
major version bump.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at
your option.
