//! Platform-aware async task spawning.
//!
//! On native: delegates to real `tokio::task`.
//! On WASM: custom dispatch — main thread vs Web Worker.
//!
//! Baseline re-exported from `tokio_with_wasm::alias::task` (provides `yield_now`,
//! `JoinHandle`, `JoinError`, etc.). On WASM, `spawn` and `spawn_blocking` are
//! overridden with Worker-lifecycle-aware versions.

pub use tokio_alias::task::*;
use tokio_with_wasm::alias as tokio_alias;

/// Spawn an async task. Under `sim-time` (native) the future is wrapped in the
/// quiescence poll-wrapper ([`crate::time::participate`]) so the spawned task
/// counts as a running participant while it is being polled — the virtual clock
/// cannot advance past an in-progress task. This is THE async-spawn chokepoint;
/// a raw `tokio::spawn` bypassing it would run uncounted and let the clock race
/// (forbidden by the `arch.no-raw-tokio-spawn` ast-grep rule). Off the sim path
/// it delegates straight to `tokio` (shadowing the glob re-export above).
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio_alias::task::spawn(crate::time::participate(future))
}

/// Cooperative async yield. Under `sim-time` (native) it participates in
/// quiescence: a busy-poll `loop { yield_now().await }` (e.g. the downloader
/// awaiting a fetch slot) must let the virtual clock advance, exactly as real
/// time passes during a yield — else the task's spawn gate pins `active_async`
/// across every self-wake and the clock freezes (a livelock). Shadows the
/// `tokio_alias::task::yield_now` glob re-export above; off the sim path that
/// real yield is used unchanged. See `crate::time::sim::SimYield`.
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub async fn yield_now() {
    crate::time::sim::yield_now().await;
}

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{JoinError, JoinHandle, spawn, spawn_blocking};
