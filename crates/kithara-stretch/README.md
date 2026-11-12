<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-stretch.svg)](https://crates.io/crates/kithara-stretch)
[![docs.rs](https://docs.rs/kithara-stretch/badge.svg)](https://docs.rs/kithara-stretch)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-stretch

Time-stretch engines behind one `ElasticEngine` contract. Each engine reports
its frame limits, latency, and whether it supports rate change and key lock
(`ElasticCapabilities`, `BackendCapabilities`), so callers plan against
capabilities rather than a library name.

- `stretch-signalsmith` (default) — Signalsmith Stretch, key lock, native only.
- `stretch-bungee` — Bungee, key lock, native only.
- `stretch-glide` — pure-Rust varispeed (pitch follows tempo), also on wasm.
- `stretch-identity` — copies samples at unity rate.

With key lock off, `build_varispeed_engine` changes duration and pitch together
through the Glide resampler.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-stretch) for the backend contract and wasm notes.
