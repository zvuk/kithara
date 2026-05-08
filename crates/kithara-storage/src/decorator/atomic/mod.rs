#![forbid(unsafe_code)]

//! Crash-safe **whole-file** write decorator.
//!
//! Production code lives in `core`; unit-tests live in `tests`.
//! See [`Atomic`] for the lifecycle contract.

pub(crate) mod core;
#[cfg(test)]
mod tests;

pub use core::Atomic;
#[cfg(not(target_arch = "wasm32"))]
pub use core::AtomicMmap;
