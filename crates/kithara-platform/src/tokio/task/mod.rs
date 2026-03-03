//! Platform-aware async task spawning.
//!
//! On native: delegates to real `tokio::task`.
//! On WASM: custom dispatch — main thread vs Web Worker.
//!
//! Baseline re-exported from `tokio_with_wasm::alias::task` (provides `yield_now`,
//! `JoinHandle`, `JoinError`, etc.). On WASM, `spawn` and `spawn_blocking` are
//! overridden with Worker-lifecycle-aware versions.

pub use tokio_with_wasm::alias::task::*;

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{JoinError, JoinHandle, spawn, spawn_blocking};
