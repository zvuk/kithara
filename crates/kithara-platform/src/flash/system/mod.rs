//! Flash engine internals. Everything outside `system/` consumes the narrow
//! forward surface re-exported below; the credit surface additionally stays
//! reachable as `system::credit::*` until the W2 RAII forms land.

/// Participant credit accounting (dedicated pacers, bridged waits, blocking
/// pacer bracket) split out of `sched` — see `credit.rs`.
pub(super) mod credit;
/// Per-task gate FSM ([`gate::TaskGate`]) — the waker-interception state
/// machine behind [`crate::flash::Participating`].
pub(super) mod gate;
/// Real-I/O pacing (the `real_io` count, pace anchor maintenance and the
/// pacer thread) — see `pace.rs` and [`crate::flash::RealIoScope`].
pub(super) mod pace;
/// Quiescence-driven virtual-clock engine. The engine drives `SIM_NANOS`
/// forward at quiescent points. Its consumers are the platform wait primitives
/// (`thread::park_timeout`, `sync::Condvar`, async `FlashSleep`/`Notify`) plus
/// the harness.
pub(super) mod sched;
/// Waiter wake handles ([`wake::Token`] / [`wake::Wake`]).
pub(super) mod wake;

pub(in crate::flash) use pace::{real_io_enter, real_io_exit};
pub(in crate::flash) use sched::{
    AsyncHandle, async_acquire, cancel_async_wait, cancel_yield, dump, next_condvar_id,
    park_timed_unparkable, register_channel_async, register_condvar_timed,
    register_condvar_untimed, register_notify_async, register_sleep_async, register_yield_async,
    reset, signal_channel, signal_condvar, signal_notify, sleep_timed, unpark, yield_until_advance,
};
