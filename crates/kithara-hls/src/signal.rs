use std::sync::OnceLock;

use kithara_platform::{
    sync::{Arc, ThreadGate, WaitGate},
    time::Duration,
};
use kithara_stream::{DeferredWake, WorkerWake};

/// Late-bound audio-worker data-arrival wake, shared across the coord, every
/// variant, and each `FetchCmd` it emits. Created empty in `Hls::create` and
/// filled once by `HlsSource::set_worker_wake` after the audio worker exists;
/// read lock-free on the off-RT write/settle path. `None` until set (the 10 ms
/// scheduler backstop covers that warm-up window) and always `None` for
/// non-audio consumers.
pub(crate) type WorkerWakeCell = Arc<OnceLock<Arc<dyn WorkerWake>>>;

/// Fire the worker wake if one is installed. Lock-free and wait-free; never
/// reached from the RT produce core. Private to this module — the only callers
/// are the [`SizeSignal`] fire methods.
fn wake_worker(cell: &WorkerWakeCell) {
    if let Some(wake) = cell.get() {
        wake.wake();
    }
}

/// Unified reader-wake handle pairing the readiness gate with the late-bound
/// audio-worker wake. Replaces the two manually-paired handles (`ready` +
/// `worker_wake`) threaded through `PlanCtx`, `HlsCoord`, and `FetchSlot`.
///
/// [`fire`](Self::fire) wakes both: the off-RT reader parked in
/// `wait_range(_, None)` (via the gate) **and** the RT decoder's audio worker
/// (via the late-bound wake). Used at the two downloader write/settle sites,
/// the dispatch/commit transitions that publish new readable bytes, and the
/// coord's seek preparation.
///
/// `Clone` shares both underlying `Arc`s — every clone signals the same gate and
/// the same worker cell.
#[derive(Clone)]
pub(crate) struct SizeSignal {
    /// Direct off-RT peer-poll wake. Unlike `peer_wake`, this invokes the
    /// downloader's registered task waker without waiting for a forwarding
    /// micro-task to be scheduled.
    peer_poll_wake: Arc<OnceLock<Arc<dyn WorkerWake>>>,
    /// Deferred peer wake used by RT callers, which arm it for the scheduler
    /// shell to flush without invoking a task waker on the produce core.
    peer_wake: Arc<OnceLock<Arc<DeferredWake>>>,
    /// Shared readiness gate. Every transition that can flip a blocked reader's
    /// `wait_range` predicate (segment write/commit/fail, fence raise/clear,
    /// seek reset, cancel) signals it; the off-RT `wait_range(_, None)` parks on
    /// it instead of polling a wall-clock timer. See `CONTEXT.md`
    /// "Event-driven read wait".
    ///
    /// Lock-free [`ThreadGate`] (atomic bump + `unpark`) rather than a condvar:
    /// readiness edges reach it from the produce core, which must not take a
    /// condvar mutex / `notify_all` futex. Single-waiter — the one off-RT
    /// `wait_range(_, None)` reader registers for the `unpark` fast-path; the
    /// counter bump alone closes the lost-wakeup window.
    ready: Arc<ThreadGate>,
    /// Late-bound audio-worker wake, fired alongside `ready` so the RT
    /// decoder's worker re-ticks on data arrival rather than on its 10 ms
    /// scheduler poll.
    worker_wake: WorkerWakeCell,
}

impl SizeSignal {
    /// Construct from a fresh readiness gate and an empty worker-wake cell. Built
    /// once in `Hls::create` and cloned down into every consumer.
    pub(crate) fn new(ready: Arc<ThreadGate>, worker_wake: WorkerWakeCell) -> Self {
        Self {
            ready,
            worker_wake,
            peer_poll_wake: Arc::new(OnceLock::new()),
            peer_wake: Arc::new(OnceLock::new()),
        }
    }

    /// Arm the peer wake from a non-blocking decoder reader. The scheduler
    /// shell flushes the stored wake off the real-time path.
    pub(crate) fn arm_peer(&self) {
        if let Some(wake) = self.peer_wake.get() {
            wake.arm();
        }
    }

    /// Signal the gate, then re-tick the audio worker. Used where newly readable
    /// bytes land or a settle/commit makes a range resolvable: per-chunk write,
    /// committed-by-race, cache-hit dispatch, terminal settle, and the
    /// variant-switch commit. Runs off-RT (downloader thread) or at coord
    /// commit; taking the gate's condvar mutex and the wait-free worker unpark
    /// are both allowed there.
    pub(crate) fn fire(&self) {
        self.ready.signal();
        wake_worker(&self.worker_wake);
    }

    /// Re-vend the underlying readiness gate. Used by the cancel waker, which
    /// must capture a hard-`Send + Sync` handle in its `on_cancel` closure.
    pub(crate) fn ready_gate(&self) -> Arc<ThreadGate> {
        Arc::clone(&self.ready)
    }

    /// Install the context-specific peer wakes (idempotent — only the first set
    /// sticks). Called by `HlsPeer::activate`.
    pub(crate) fn set_peer_wake(&self, deferred: Arc<DeferredWake>, direct: Arc<dyn WorkerWake>) {
        let _ = self.peer_wake.set(deferred);
        let _ = self.peer_poll_wake.set(direct);
    }

    /// Install the audio worker's data-arrival wake (idempotent — only the first
    /// set sticks). Called by the coord once the worker exists; the fire methods
    /// read it lock-free thereafter.
    pub(crate) fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>) {
        let _ = self.worker_wake.set(wake);
    }

    /// Wake the HLS peer's `poll_next` directly from an off-RT caller. This
    /// covers both slow-fetch escape and incoming-session readiness work. A
    /// no-op until the peer activates.
    pub(crate) fn wake_peer(&self) {
        if let Some(wake) = self.peer_poll_wake.get() {
            wake.wake();
        }
    }

    /// Re-tick the audio worker after publishing work that it owns, without
    /// claiming that reader bytes or readiness changed.
    pub(crate) fn wake_worker(&self) {
        wake_worker(&self.worker_wake);
    }

    delegate::delegate! {
        to self.ready {
            /// Pre-park snapshot of the gate generation (seqlock guard for the off-RT
            /// `wait_range` loop). Pass-through to [`WaitGate::current`].
            pub(crate) fn current(&self) -> u64;
            /// Park on the gate until its generation advances past `since` or `timeout`
            /// elapses. Returns `true` on a signal, `false` on timeout. Pass-through to
            /// [`WaitGate::wait_timeout`].
            pub(crate) fn wait_timeout(&self, since: u64, timeout: Duration) -> bool;
        }
    }
}
