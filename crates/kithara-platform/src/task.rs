//! Platform-aware blocking task spawning.
//!
//! [`spawn_blocking`] is the chokepoint for offloading a blocking computation
//! onto a runtime thread. It exists so that, under the `flash-time` test feature,
//! the offloaded closure participates in the quiescence clock the same way a
//! named thread does — its credit is reset on entry and dropped on exit by the
//! same bracket [`crate::thread::spawn_named`] uses. Consumers that run a
//! wrapped wait (`thread::park_timeout`, `sync::Condvar`) on a blocking thread
//! must spawn through this wrapper instead of `tokio::task::spawn_blocking`, so
//! participant accounting stays intrinsic to the platform — no consumer ever
//! registers anything.

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
use crate::time::flash::sched;

/// Spawn a blocking computation on the runtime's blocking pool.
///
/// Off the sim path: a thin pass-through to [`tokio::task::spawn_blocking`].
/// Under `flash-time` (native): brackets the closure with `reset_credit()` on
/// entry and `on_participant_exit()` on exit, so a closure that woke to
/// `Running` inside a wrapped wait releases its quiescence-engine slot when it
/// returns (and a reused pool thread never inherits a stale credit).
///
/// The parent's ambient snapshot is also re-established on the blocking thread
/// for the closure's lifetime (thread-locals do not cross the pool), so a
/// blocking computation spawned from a flash test stays flash-eligible.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let ambient = crate::time::flash::ambient_snapshot();
    tokio::task::spawn_blocking(move || {
        // Held for the closure's lifetime (must outlive `f()`); restores the
        // pool thread's previous ambient on exit.
        let _ambient = crate::time::flash::set_ambient_for_spawn(ambient);
        // TODO(flash C3): a prod `#[flash(true)]` closure (the `Audio::new`
        // decode probe) should ALSO eagerly count this blocking thread as an
        // `active` engine participant while it runs — that engine-accounting
        // refinement is deferred to C3. B5 only propagates the ambient flag.
        sched::reset_credit();
        let result = f();
        sched::on_participant_exit();
        result
    })
}

/// Spawn a blocking computation on the runtime's blocking pool (non-sim native).
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
pub fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
}

/// Spawn a blocking computation (wasm): delegates to the platform tokio shim.
#[cfg(target_arch = "wasm32")]
pub use crate::tokio::task::spawn_blocking;
