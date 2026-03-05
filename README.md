<div align="center">
  <img src="logo.svg" alt="kithara" width="360">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/zvuk/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/zvuk/kithara)
[![Crates.io](https://img.shields.io/crates/v/kithara.svg)](https://crates.io/crates/kithara)
[![docs.rs](https://docs.rs/kithara/badge.svg)](https://docs.rs/kithara)

</div>

# kithara

Modular Rust audio stack for progressive HTTP and HLS playback, shared by desktop and wasm players.

Status: early-stage APIs with active internal refactoring. The architecture is not being "simplified" by removing layers; it is being improved by making crate boundaries clearer and contracts stricter.

## Workspace map

Core runtime:
- [`kithara`](crates/kithara/README.md) facade API
- [`kithara-play`](crates/kithara-play/README.md) player engine
- [`kithara-audio`](crates/kithara-audio/README.md) audio pipeline
- [`kithara-decode`](crates/kithara-decode/README.md) decoders
- [`kithara-stream`](crates/kithara-stream/README.md) async-to-sync bridge
- [`kithara-storage`](crates/kithara-storage/README.md) storage resources
- [`kithara-assets`](crates/kithara-assets/README.md) cache store

Protocol layer:
- [`kithara-file`](crates/kithara-file/README.md) progressive download
- [`kithara-hls`](crates/kithara-hls/README.md) HLS orchestration
- [`kithara-abr`](crates/kithara-abr/README.md) ABR logic
- [`kithara-drm`](crates/kithara-drm/README.md) segment decryption
- [`kithara-net`](crates/kithara-net/README.md) HTTP/retry/timeout

Platform and infra:
- [`kithara-platform`](crates/kithara-platform/README.md) cross-platform primitives
- [`kithara-bufpool`](crates/kithara-bufpool/README.md) pooled buffers
- [`kithara-events`](crates/kithara-events/README.md) event bus
- [`kithara-hang-detector`](crates/kithara-hang-detector/README.md) watchdog guards

Apps and UIs:
- [`kithara-app`](crates/kithara-app/README.md) app entrypoints and binaries
- [`kithara-ui`](crates/kithara-ui/README.md) desktop GUI layer
- [`kithara-tui`](crates/kithara-tui/README.md) terminal UI layer
- [`kithara-wasm`](crates/kithara-wasm/README.md) browser player

Macros and tests:
- [`kithara-wasm-macros`](crates/kithara-wasm-macros/README.md)
- [`kithara-test-macros`](crates/kithara-test-macros/README.md)
- [`kithara-test-utils`](crates/kithara-test-utils/README.md)
- [`tests`](tests/README.md) integration/perf/e2e suites

## Quick start

```bash
cargo build --workspace
cargo install just --locked
just lint-fast
just test-all
```

## Demo entrypoints

Desktop:
```bash
cargo run -p kithara-app --bin kithara-gui
cargo run -p kithara-app --bin kithara-tui
```

WASM player:
```bash
cd crates/kithara-wasm
RUSTUP_TOOLCHAIN=nightly trunk serve --config Trunk.toml
```

## Contributing

- Development guide: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Workspace coding rules: [`AGENTS.md`](AGENTS.md)

## License

Dual-licensed under MIT or Apache-2.0.
