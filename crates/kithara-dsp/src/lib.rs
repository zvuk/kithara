//! Allocation-free, panic-free vector DSP over planar `f32`, without implicit
//! sanitization. Layout kernels match `fast_interleave`; strided channel-major
//! operations need no plane-reference slices. Apple uses Accelerate; other targets
//! select `fearless_simd` by CPU capability. `filter` supplies denormal-safe biquads,
//! `interp` supplies fractional sampling and rate ramps, and feature-gated
//! `spectrum` supplies Hann-windowed real FFT, bin magnitude/phase and
//! autocorrelation. `sum_squares` reduces energy; `downmix` averages channels.
//! Element-wise kernels (`ramp`, `div`, `affine`, `sub`, `scale_add`,
//! `mul_add`, `add`) and `fade::gains` compose vector fades.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

mod backend;
mod consts;
/// Fade curves: one lexicon for firewheel's `MixDSP` and every crossfade gain.
pub mod fade;
/// Biquad cascades over channel planes and the designs that feed them.
pub mod filter;
/// Window interpolation at fractional positions and the rate ramps that
/// place them.
pub mod interp;
mod layout;
/// Parameter smoothing and A/B mixing owned by firewheel, re-exported as the
/// one import path the workspace uses; a re-export can later become a local
/// type of the same name without touching consumers.
pub mod param;
/// Real FFT lengths every backend runs; with the `spectrum` feature, the real
/// FFT over a Hann window, the magnitude and phase of its bins, and the
/// autocorrelation of a frame.
pub mod spectrum;
mod vector;

pub use backend::sanitize;
pub use layout::{
    deinterleave_channel_major, deinterleave_variable, downmix, interleave_channel_major,
};
pub use vector::{add, affine, crossfade_linear, div, mul_add, ramp, scale_add, sub, sum_squares};
