use std::{future::Future, panic::Location};

pub use crate::{
    backend::tokio::task::{JoinError, JoinHandle},
    flash::yield_now,
};
use crate::{
    backend::tokio::{backend::task, runtime::Handle, task as native_task},
    flash::system::{
        credit,
        credit::{DedicatedSlot, Participant},
    },
    maybe_send::MaybeSend,
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
    task::spawn(crate::flash::with_ambient(
        on,
        crate::flash::participate(crate::no_block::watch_blanket_at("spawn", loc, future), loc),
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
        crate::flash::participate(crate::no_block::watch_blanket_at("spawn", loc, future), loc),
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
#[track_caller]
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let origin = Location::caller();
    let ambient = crate::flash::ambient_snapshot();
    let slot = ambient.then(|| DedicatedSlot::reserve(origin));
    native_task::spawn_blocking(move || {
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        credit::reset_credit();
        if let Some(slot) = slot {
            let _pacer = slot.claim_pooled();
            f()
        } else {
            let _exit = Participant::unreserved();
            f()
        }
    })
}

/// Spawn synchronous work without blocking an async runtime worker.
#[track_caller]
pub fn spawn_sync<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + MaybeSend + 'static,
    R: MaybeSend + 'static,
{
    spawn_blocking(f)
}

/// Spawn a blocking computation on a specific runtime [`Handle`].
///
/// Same ambient propagation and quiescence accounting as [`spawn_blocking`],
/// but queued onto the captured runtime handle.
///
/// Reserves the `active` slot before the pool queues the closure, covering the queue wait; the
/// slot's `Drop` returns the reservation if the pool never runs it.
#[track_caller]
pub fn spawn_blocking_on<F, R>(handle: &Handle, f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let origin = Location::caller();
    let ambient = crate::flash::ambient_snapshot();
    let slot = ambient.then(|| DedicatedSlot::reserve(origin));
    handle.spawn_blocking(move || {
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        credit::reset_credit();
        if let Some(slot) = slot {
            let _pacer = slot.claim_pooled();
            f()
        } else {
            let _exit = Participant::unreserved();
            f()
        }
    })
}
