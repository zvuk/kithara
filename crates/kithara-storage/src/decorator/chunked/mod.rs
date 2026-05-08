#![forbid(unsafe_code)]

//! Crash-safe **chunked** write decorator.
//!
//! Production code lives in `core`; unit-tests live in `tests`.
//! See [`AtomicChunked`] for the lifecycle contract.

pub(crate) mod core;
#[cfg(test)]
mod tests;

pub use core::{AtomicChunked, OpenIntent};
