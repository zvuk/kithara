//! Platform-aware async task spawning.
//!
//! On native: delegates to real `tokio::task`.
//! On WASM: custom dispatch — main thread vs Web Worker.
//!
//! Baseline re-exported from `tokio_with_wasm::alias::task` (provides `yield_now`,
//! `JoinHandle`, `JoinError`, etc.). On WASM, `spawn` and `spawn_blocking` are
//! overridden with Worker-lifecycle-aware versions.

pub use tokio_with_wasm::alias::task::*;

// Under `flash-time` (native) [`spawn`] wraps the future in the quiescence
// poll-wrapper and `yield_now` participates in quiescence UNDER AMBIENT (a
// flash(true) test's busy-poll `loop { yield_now().await }` must let the virtual
// clock advance). Like the stateful sync primitives it branches on
// `flash_ambient`: in a flash(false) test / production it is a plain scheduler
// yield, so the flash-time build stays behavior-transparent (an engine-backed
// yield could never be granted there — the surrounding task keeps its
// `active_async` slot across the yield while its other primitives are real). Both
// shadow the `tokio_with_wasm::alias::task` glob re-export above; off the sim path
// the real `tokio` spawn/yield are used unchanged. See `crate::time::flash`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
mod spawn;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub use spawn::spawn;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub use crate::time::flash::yield_now;

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{JoinError, JoinHandle, spawn, spawn_blocking};
