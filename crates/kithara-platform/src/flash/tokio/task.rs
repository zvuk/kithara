use std::{future::Future, panic::Location};

// Under `flash` (native) [`spawn`] wraps the future in the quiescence
// poll-wrapper and `yield_now` participates in quiescence UNDER AMBIENT (a
// flash(true) test's busy-poll `loop { yield_now().await }` must let the virtual
// clock advance). Like the stateful sync primitives it keys on
// `flash_ambient` (consulted per call — a yield has no cross-thread signal
// partner): in a flash(false) test / production it is a plain scheduler
// yield, so the flash build stays behavior-transparent (an engine-backed
// yield could never be granted there — the surrounding task keeps its
// `active_async` slot across the yield while its other primitives are real).
// Off the sim path the real `tokio` spawn/yield are used unchanged. See
// `crate::flash`.
pub use crate::flash::yield_now;
pub use crate::native::tokio::task::{JoinError, JoinHandle};
use crate::{
    flash::system::credit,
    native::tokio::{runtime::Handle, task as native_task},
};

/// Spawn an async task. Under `flash` (native) the future is wrapped in the
/// quiescence poll-wrapper ([`crate::flash::participate`]) so the spawned task
/// counts as a running participant while it is being polled — the virtual clock
/// cannot advance past an in-progress task. This is THE async-spawn chokepoint;
/// a raw `tokio::spawn` bypassing it would run uncounted and let the clock race.
/// A raw `tokio::spawn` needs a direct `tokio` dependency, which the
/// `arch.tokio_dep_quarantine` xtask check confines to this crate — so consumers
/// must route through the platform re-export and reach this chokepoint. Off the
/// sim path it delegates straight to the native `tokio` spawn.
///
/// The future is also wrapped in [`crate::flash::with_ambient`] carrying
/// the parent's ambient snapshot, re-asserted per-poll so the task sees the
/// test's flash-eligibility gate even when tokio moves it between worker threads
/// (thread-locals do not cross `spawn`). The ambient wrap is OUTER so both
/// `participate`'s accounting and the task body run under the asserted ambient.
#[track_caller]
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let on = crate::flash::ambient_snapshot();
    let loc = Location::caller();
    native_task::spawn(crate::flash::with_ambient(
        on,
        crate::flash::participate(future, loc),
    ))
}

/// Spawn a future on a SPECIFIC runtime [`Handle`] through the chokepoint.
/// Same quiescence + ambient wrapping as [`spawn`], but
/// onto a stored runtime handle rather than the implicit current runtime — for
/// orchestrators (e.g. the downloader run loop) that own their runtime. A raw
/// `handle.spawn(fut)` here would run UNCOUNTED and let the virtual clock race
/// past the orchestrator's event waits, freezing the clock.
#[track_caller]
pub fn spawn_on<F>(handle: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let on = crate::flash::ambient_snapshot();
    let loc = Location::caller();
    handle.spawn(crate::flash::with_ambient(
        on,
        crate::flash::participate(future, loc),
    ))
}

/// Spawn a blocking computation on the runtime's blocking pool.
///
/// Off the sim path: a thin pass-through to [`tokio::task::spawn_blocking`].
/// Under `flash` (native), an AMBIENT closure is real work in flight, so
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
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let ambient = crate::flash::ambient_snapshot();
    // Reserve the `active` slot BEFORE the pool queues the closure (covering
    // the queue wait). The slot's Drop returns the reservation if the pool
    // never runs the closure. Deliberately NOT unified with the `spawn_named`
    // bracket: a pool closure owns no named-thread count and must restore the
    // reused thread's previous dedicated flag.
    let slot = ambient.then(credit::DedicatedSlot::reserve);
    native_task::spawn_blocking(move || {
        // Held for the closure's lifetime (must outlive `f()`); restores the
        // pool thread's previous ambient on exit.
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        credit::reset_credit();
        if let Some(slot) = slot {
            let _pacer = slot.claim_pooled();
            f()
        } else {
            // Non-ambient: invisible to the engine; the RAII settle only keeps
            // the exit unwind-safe and consistent with the ambient arm.
            let _exit = credit::Participant::unreserved();
            f()
        }
    })
}
