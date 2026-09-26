//! Vector DSP kernels over planar `f32` slices.
//!
//! [`Platform`] picks the backend for the build target: [`Accelerate`] on
//! Apple, [`Portable`] (`fearless_simd`, runtime-selected SIMD level) elsewhere.
//! Kernels process the common prefix of their slices, return the frame count
//! they handled, never allocate and never sanitize implicitly.
#![forbid(unsafe_code)]

mod backend;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use backend::Accelerate;
pub use backend::{Backend, Platform, Portable};
