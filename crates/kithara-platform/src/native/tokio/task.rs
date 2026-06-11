//! Platform-aware async task spawning (native, off the sim path).
//!
//! Baseline re-exported from `tokio_with_wasm::alias::task` (provides `spawn`,
//! `yield_now`, `JoinHandle`, `JoinError`, etc.). The `flash` build replaces
//! this module with `crate::flash::tokio::task` behind the `crate::tokio::task`
//! facade.

use tokio::runtime::Handle;
pub use tokio_with_wasm::alias::task::*;

/// Spawn `future` on a specific runtime [`Handle`] (off-sim: a direct
/// `handle.spawn`).
///
/// Without `flash` there is no quiescence engine, so spawning onto a
/// specific runtime [`Handle`] needs no wrapping. The `flash` build uses the
/// flash `spawn_on` instead, which additionally wraps the future in the engine
/// poll-wrapper + ambient snapshot so the orchestrator's event waits stay
/// counted by the virtual clock.
pub fn spawn_on<F>(handle: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle.spawn(future)
}

/// Spawn a blocking computation on the runtime's blocking pool (non-sim native).
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
}
