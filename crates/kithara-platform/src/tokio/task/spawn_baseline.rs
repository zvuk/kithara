//! Off-sim baseline [`spawn_on`]: a plain `handle.spawn`.
//!
//! Without `flash` there is no quiescence engine, so spawning onto a
//! specific runtime [`Handle`] needs no wrapping. The `flash` build uses
//! [`super::spawn::spawn_on`] instead, which additionally wraps the future in
//! the engine poll-wrapper + ambient snapshot so the orchestrator's event waits
//! stay counted by the virtual clock.

use tokio::{runtime::Handle, task::JoinHandle};

/// Spawn `future` on a specific runtime [`Handle`] (off-sim: a direct
/// `handle.spawn`).
pub fn spawn_on<F>(handle: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle.spawn(future)
}
