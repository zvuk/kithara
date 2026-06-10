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
use crate::time::flash::credit;

/// Spawn a blocking computation on the runtime's blocking pool.
///
/// Off the sim path: a thin pass-through to [`tokio::task::spawn_blocking`].
/// Under `flash-time` (native), an AMBIENT closure is real work in flight, so
/// it paces the virtual clock exactly like a `spawn_named` thread: the caller
/// reserves the `active` slot BEFORE the pool queues the closure (covering the
/// queue wait), the closure claims it `Running` for its lifetime, and its
/// engine parks release it as usual — the clock advances while the closure
/// WAITS, never while it runs or sits queued. Without this the clock outruns
/// the closure's real execution and virtual deadlines fire against time the
/// work never had. A non-ambient closure stays invisible to the engine.
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
    if ambient {
        credit::pre_count_dedicated();
    }
    tokio::task::spawn_blocking(move || {
        // Held for the closure's lifetime (must outlive `f()`); restores the
        // pool thread's previous ambient on exit.
        let _ambient = crate::time::flash::set_ambient_for_spawn(ambient);
        credit::reset_credit();
        if ambient {
            let _pacer = credit::BlockingPacer::enter();
            f()
        } else {
            let result = f();
            credit::on_participant_exit();
            result
        }
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
