//! Vector DSP kernels over planar `f32` slices.
//!
//! [`Platform`] picks the backend for the build target: [`Accelerate`] on
//! Apple, [`Portable`] (`fearless_simd`, runtime-selected SIMD level) elsewhere.
//! Kernels process the common prefix of their slices, return the frame count
//! they handled, never allocate and never sanitize implicitly.
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

mod backend;
/// Fade curves: one lexicon for firewheel's `MixDSP` and every crossfade gain.
pub mod fade;
/// Parameter smoothing and A/B mixing owned by firewheel, re-exported as the
/// one import path the workspace uses; a re-export can later become a local
/// type of the same name without touching consumers.
pub mod param;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use backend::Accelerate;
pub use backend::{Backend, Platform, Portable};
