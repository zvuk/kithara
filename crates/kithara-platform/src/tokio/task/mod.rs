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
pub use spawn::{spawn, spawn_on};

// Under `flash-time`, route `spawn_blocking` through the ambient-propagating
// wrapper (`crate::task::spawn_blocking`) so a blocking closure spawned from a
// flash test carries `FLASH_AMBIENT` onto the pool thread for its lifetime —
// a wrapped wait (`park_timeout`, `Condvar`) on that thread then stays virtual,
// matching the engine instead of parking on the real clock the engine cannot
// wake. Shadows the `tokio_with_wasm::alias::task` glob `spawn_blocking` above,
// exactly as `spawn`/`spawn_on`/`yield_now` shadow their glob counterparts. Off
// the sim path the raw glob `spawn_blocking` is used unchanged.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub use crate::task::spawn_blocking;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub use crate::time::flash::yield_now;

// Off-sim baseline `spawn_on` (plain `handle.spawn`) — its own module so this
// `mod.rs` stays declarations + re-exports only (style.no-items-in-lib-or-mod-rs).
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
mod spawn_baseline;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
pub use spawn_baseline::spawn_on;

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{JoinError, JoinHandle, spawn, spawn_blocking};
