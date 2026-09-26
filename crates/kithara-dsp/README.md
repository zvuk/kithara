<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-dsp.svg)](https://crates.io/crates/kithara-dsp)
[![docs.rs](https://docs.rs/kithara-dsp/badge.svg)](https://docs.rs/kithara-dsp)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-dsp

Vector DSP kernels over planar `f32` slices. Apple builds run them on
Accelerate; every other target runs them on `fearless_simd` at the best SIMD
level the CPU reports. Each kernel handles the common prefix of its slices,
returns the frame count it handled, and never allocates.

## Usage

```rust
use kithara_dsp::{Backend, Platform};

let backend = Platform::default();
let mut interleaved = [0.0_f32; 4];
assert_eq!(backend.interleave_pair(&[1.0, 2.0], &[-1.0, -2.0], &mut interleaved), 2);
assert_eq!(interleaved.map(f32::to_bits), [1.0_f32, -1.0, 2.0, -2.0].map(f32::to_bits));
```

## Key Types

<table>

<tr><th>Type</th><th>Role</th></tr>

<tr><td><code>Backend</code></td><td>Sealed kernel contract every backend implements</td></tr>

<tr><td><code>Platform</code></td><td>The backend the build target uses by default</td></tr>

<tr><td><code>Portable</code></td><td><code>fearless_simd</code> kernels at a SIMD level fixed at construction</td></tr>

<tr><td><code>Accelerate</code></td><td>vDSP and BLAS kernels through <code>kithara-apple</code>; Apple only</td></tr>

</table>

## Integration

`kithara-signal` and `kithara-decode` call the layout kernels. Owners create a
backend once and keep it as a field, so the SIMD level is chosen once and never
on the hot path.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-dsp) for detailed contracts, invariants, and internals.
