//! Flash engine internals. [`FlashInner`] (`inner.rs`) is the SINGLE owner of
//! all engine state: the virtual clock ([`inner::Clock`]), the one
//! lock-protected scheduler core ([`inner::Core`], split into
//! [`inner::Registry`] + [`inner::Scheduler`] data) and the real-I/O pacer
//! (`pace::Pacer`, with its lazily-spawned eternal thread holding a strong
//! `Arc` to its OWN instance). The process engine is the lazily created
//! [`FLASH`] instance.
//!
//! Engine methods are instance-addressed (`&self` on `FlashInner`); nothing
//! below `FlashInner` reaches for the [`FLASH`] global, so local instances
//! behave identically. Everything outside `system/` consumes thin free-fn
//! forwards (each one `FLASH.method(...)`) re-exported below.
//!
//! The pure-scheduler tests in `flash/tests.rs` run on LOCAL instances
//! (`FlashInner::new_arc`, cfg(test)) — only the primitive-path tests (and
//! production) drive the global [`FLASH`] through the forwards.

/// Participant credit accounting (dedicated pacers, bridged waits, blocking
/// pacer bracket) split out of the scheduler — see `credit.rs`.
pub(super) mod credit;
/// Per-task gate FSM ([`gate::TaskGate`]) — the waker-interception state
/// machine behind [`crate::flash::Participating`].
pub(super) mod gate;
/// The engine-state owner: `FlashInner` + `Clock` + `Core{Registry, Scheduler}`
/// + the `FLASH` process instance.
pub(super) mod inner;
/// Real-I/O pacing (the `real_io` count, pace anchor maintenance and the
/// per-instance pacer thread) — see `pace.rs` and [`crate::flash::RealIoScope`].
pub(super) mod pace;
/// Quiescence-driven virtual-clock mechanics: the advance rule plus the
/// `register_*`/`signal_*`/park surface, as `FlashInner` methods. Consumers
/// are the platform wait primitives (`thread::park_timeout`, `sync::Condvar`,
/// async `FlashSleep`/`Notify`) plus the harness.
pub(super) mod sched;
/// Waiter wake handles ([`wake::Token`] / [`wake::Wake`]).
pub(super) mod wake;

pub(in crate::flash) use inner::{
    Clock, Core, CvId, FLASH, FlashInner, Registry, SyncHolder, WaiterId,
};
pub(in crate::flash) use pace::{real_io_enter, real_io_exit};
pub(in crate::flash) use sched::{
    AsyncHandle, async_acquire, cancel_async_wait, cancel_yield, dump, next_condvar_id,
    park_timed_unparkable, register_channel_async, register_condvar_timed,
    register_condvar_untimed, register_notify_async, register_sleep_async, register_yield_async,
    signal_channel, signal_condvar, signal_notify, sleep_timed, unpark, yield_until_advance,
};
