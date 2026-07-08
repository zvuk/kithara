<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-resampler.svg)](https://crates.io/crates/kithara-resampler)
[![docs.rs](https://docs.rs/kithara-resampler/badge.svg)](https://docs.rs/kithara-resampler)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-resampler

Sample-rate resampler contracts and backend adapters for Kithara.

This crate owns the resampler backend trait, capabilities, construction config,
error types, and standalone PCM-to-PCM backend implementations.
Decoder placement and playback graph routing stay in `kithara-decode` and
`kithara-audio`; those crates import this crate instead of owning backend
implementations.

Every resampler is built from explicit BON config. Built-in and custom backends
all implement `ResamplerBackend`; the config carries the backend object/factory
directly. Backends do not choose another backend when a requested mode is
unavailable. Hot paths use caller-owned buffers or scratch from an injected
`kithara-bufpool::PcmPool`; library code must not create a hidden default pool.

The current built-in backend is feature-gated explicitly:

- `resample-rubato` enables the Rubato backend; its algorithm is selected by
  `rubato::RubatoConfig`.

See [CONTEXT.md](CONTEXT.md) for the backend contract, allocation contract, and
decoder integration rules.
