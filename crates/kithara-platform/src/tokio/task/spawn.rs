//! Native `flash-time` task spawning and cooperative yield.

use std::future::Future;

use tokio_with_wasm::alias as tokio_alias;

use super::JoinHandle;

/// Spawn an async task. Under `flash-time` (native) the future is wrapped in the
/// quiescence poll-wrapper ([`crate::time::participate`]) so the spawned task
/// counts as a running participant while it is being polled — the virtual clock
/// cannot advance past an in-progress task. This is THE async-spawn chokepoint;
/// a raw `tokio::spawn` bypassing it would run uncounted and let the clock race
/// (forbidden by the `arch.no-raw-tokio-spawn` ast-grep rule). Off the sim path
/// it delegates straight to `tokio` (shadowing the glob re-export above).
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio_alias::task::spawn(crate::time::participate(future))
}
