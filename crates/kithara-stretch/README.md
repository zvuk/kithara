# kithara-stretch

Pure time-stretch DSP contracts and backend adapters for Kithara.

This crate owns the `StretchBackend` trait, backend selector, backend factory,
and native C++ backend adapters. It has no runtime dependency on other
`kithara-*` crates; audio graph plumbing, region planning, PCM pools, chunk
metadata, and resampler routing stay in `kithara-audio`.

Feature flags select the compiled backends:

- `stretch-signalsmith` enables `signalsmith-stretch` and is the default.
- `stretch-bungee` enables `bungee-rs` as an opt-in backend.

Both current backends are native-only. See [CONTEXT.md](CONTEXT.md) for the
backend contract, wasm notes, and the future pure-Rust backend recipe.
