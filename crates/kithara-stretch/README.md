<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-stretch.svg)](https://crates.io/crates/kithara-stretch)
[![docs.rs](https://docs.rs/kithara-stretch/badge.svg)](https://docs.rs/kithara-stretch)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-stretch

Pure time-stretch DSP contracts and backend adapters for Kithara.

This crate owns the `StretchBackend` trait, backend selector, backend factory,
and native C++ backend adapters. Backend features depend downward on
`kithara-bufpool` for scratch storage, and native builds include
`kithara-workspace-hack`; audio graph plumbing, region planning, chunk metadata,
and resampler routing stay in `kithara-audio`.

Feature flags select the compiled backends:

- `stretch-signalsmith` enables `signalsmith-stretch` and is the default.
- `stretch-bungee` enables `bungee-rs` as an opt-in backend.

Both current backends are native-only. See [CONTEXT.md](CONTEXT.md) for the
backend contract, wasm notes, and the future pure-Rust backend recipe.
