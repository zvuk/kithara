<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-resampler.svg)](https://crates.io/crates/kithara-resampler)
[![docs.rs](https://docs.rs/kithara-resampler/badge.svg)](https://docs.rs/kithara-resampler)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-resampler

Sample-rate conversion backends behind one `ResamplerBackend` trait:

- `resample-rubato` — Rubato; the algorithm is chosen by `rubato::RubatoConfig`.
- `resample-glide` — Glide: fixed ratio, variable ratio, and gliding between
  ratios (`glide::GlideConfig`), on `kithara-dsp` kernels.
- Apple AudioConverter on macOS and iOS.

The backend is part of the type at the call site (`B: ResamplerBackend`);
nothing picks a default or substitutes another backend at run time. Hot paths
use caller buffers or scratch from the caller's `PoolRegion`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-resampler) for the backend contract, allocation contract, and
decoder integration rules.
