pub use tokio_with_wasm::alias::task::*;

use super::runtime::Handle;

/// Spawn `future` on a specific runtime [`Handle`]: a direct `handle.spawn`.
///
/// The native backend has no orchestration around task polls, so spawning
/// onto a specific runtime [`Handle`] needs no wrapping.
pub fn spawn_on<F>(handle: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle.spawn(future)
}

/// Spawn a blocking computation on the runtime's blocking pool.
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
}
