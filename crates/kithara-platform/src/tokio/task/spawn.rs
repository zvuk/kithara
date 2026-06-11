//! Native `flash` task spawning and cooperative yield.

use std::future::Future;

use tokio_with_wasm::alias as tokio_alias;

use super::JoinHandle;

/// Spawn an async task. Under `flash` (native) the future is wrapped in the
/// quiescence poll-wrapper ([`crate::flash::participate`]) so the spawned task
/// counts as a running participant while it is being polled — the virtual clock
/// cannot advance past an in-progress task. This is THE async-spawn chokepoint;
/// a raw `tokio::spawn` bypassing it would run uncounted and let the clock race
/// (forbidden by the `arch.no-raw-tokio-spawn` ast-grep rule). Off the sim path
/// it delegates straight to `tokio` (shadowing the glob re-export above).
///
/// The future is also wrapped in [`crate::flash::with_ambient`] carrying
/// the parent's ambient snapshot, re-asserted per-poll so the task sees the
/// test's flash-eligibility gate even when tokio moves it between worker threads
/// (thread-locals do not cross `spawn`). The ambient wrap is OUTER so both
/// `participate`'s accounting and the task body run under the asserted ambient.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let on = crate::flash::ambient_snapshot();
    tokio_alias::task::spawn(crate::flash::with_ambient(
        on,
        crate::flash::participate(future),
    ))
}

/// Spawn a future on a SPECIFIC runtime [`Handle`](tokio::runtime::Handle)
/// through the chokepoint. Same quiescence + ambient wrapping as [`spawn`], but
/// onto a stored runtime handle rather than the implicit current runtime — for
/// orchestrators (e.g. the downloader run loop) that own their runtime. A raw
/// `handle.spawn(fut)` here would run UNCOUNTED and let the virtual clock race
/// past the orchestrator's event waits, freezing the clock.
pub fn spawn_on<F>(handle: &tokio::runtime::Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let on = crate::flash::ambient_snapshot();
    handle.spawn(crate::flash::with_ambient(
        on,
        crate::flash::participate(future),
    ))
}
