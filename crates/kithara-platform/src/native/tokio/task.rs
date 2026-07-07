use std::panic::Location;

pub use super::backend::task::*;
use super::runtime::Handle;

/// Spawn a future on the current runtime through the platform chokepoint.
#[track_caller]
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let loc = Location::caller();
    super::backend::task::spawn(crate::no_block::watch_blanket_at("spawn", loc, future))
}

/// Spawn `future` on a specific runtime [`Handle`] through the platform chokepoint.
#[track_caller]
pub fn spawn_on<F>(handle: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let loc = Location::caller();
    handle.spawn(crate::no_block::watch_blanket_at("spawn", loc, future))
}

/// Spawn a blocking computation on the runtime's blocking pool.
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    super::backend::task::spawn_blocking(f)
}

/// Spawn a blocking computation on a specific runtime [`Handle`].
pub fn spawn_blocking_on<F, R>(handle: &Handle, f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    handle.spawn_blocking(f)
}
