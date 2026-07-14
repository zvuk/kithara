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
/// - [`fire`](Self::fire) wakes both: the off-RT reader parked in
///   `wait_range(_, None)` (via the gate) **and** the RT decoder's audio worker
///   (via the late-bound wake). Used at the two downloader write/settle sites
///   and the dispatch/commit transitions that publish new readable bytes.
/// - [`fire_ready_only`](Self::fire_ready_only) wakes just the gate. Used at the
///   coord's RT-reachable fence/seek/cancel transitions, which deliberately do
///   not re-tick the worker (the 10 ms scheduler poll covers them).
///
/// `Clone` shares both underlying `Arc`s — every clone signals the same gate and
/// the same worker cell.
#[derive(Clone)]
pub(crate) struct SizeSignal {
    /// Late-bound peer-poll wake — the `HlsPeer`'s `reader_advanced` handle.
    /// Fired by [`wake_peer`](Self::wake_peer) from the `on_slow` hook when an
    /// in-flight fetch crosses `soft_timeout` without settling: the peer re-polls
    /// and `reconcile_escape` marks the ABR escape against the now-stalled slot.
    /// This is the anti-event twin of `worker_wake`/`ready` (data is *not*
    /// arriving), so the escape is edge-triggered by the stall rather than left
    /// to an incidental reader-progress wake. Empty until the peer activates
    /// (`HlsCoord::set_peer_wake`), then set once.
    peer_wake: Arc<OnceLock<Arc<DeferredWake>>>,
    /// Shared readiness gate. Every transition that can flip a blocked reader's
    /// `wait_range` predicate (segment write/commit/fail, fence raise/clear,
    /// seek reset, cancel) signals it; the off-RT `wait_range(_, None)` parks on
    /// it instead of polling a wall-clock timer. See `CONTEXT.md`
    /// "Event-driven read wait".
    ///
    /// Lock-free [`ThreadGate`] (atomic bump + `unpark`) rather than a condvar:
    /// the RT-reachable readiness edges (`fire_ready_only` from the coord's
    /// fence-clear / seek transitions) `signal` it on the produce core, which
    /// must not take a condvar mutex / `notify_all` futex. Single-waiter — the
    /// one off-RT `wait_range(_, None)` reader registers for the `unpark`
    /// fast-path; the counter bump alone closes the lost-wakeup window.
    ready: Arc<ThreadGate>,
    /// Late-bound audio-worker wake. Fired alongside `ready` on the two
    /// downloader write/settle sites (NOT the coord's RT-reachable fence/seek
    /// signals), so the RT decoder's worker re-ticks on data arrival rather than
    /// on its 10 ms scheduler poll.
    worker_wake: WorkerWakeCell,
}

impl SizeSignal {
    /// Construct from a fresh readiness gate and an empty worker-wake cell. Built
    /// once in `Hls::create` and cloned down into every consumer.
    pub(crate) fn new(ready: Arc<ThreadGate>, worker_wake: WorkerWakeCell) -> Self {
        Self {
            ready,
            worker_wake,
            peer_wake: Arc::new(OnceLock::new()),
        }
    }

    delegate::delegate! {
        to self.ready {
            /// Pre-park snapshot of the gate generation (seqlock guard for the off-RT
            /// `wait_range` loop). Pass-through to [`WaitGate::current`].
            pub(crate) fn current(&self) -> u64;
            /// Signal only the gate. Used at the coord's RT-reachable transitions
            /// (fence clear, seek reset, cancel waker) that flip a blocked reader's
            /// predicate without producing new bytes — the audio worker re-discovers the
            /// state on its next scheduler poll, so it is deliberately not re-ticked.
            #[call(signal)]
            pub(crate) fn fire_ready_only(&self);
            /// Park on the gate until its generation advances past `since` or `timeout`
            /// elapses. Returns `true` on a signal, `false` on timeout. Pass-through to
            /// [`WaitGate::wait_timeout`].
            pub(crate) fn wait_timeout(&self, since: u64, timeout: Duration) -> bool;
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

    /// Install the peer's `reader_advanced` wake (idempotent — only the first
    /// set sticks). Called by `HlsPeer::activate`; [`wake_peer`](Self::wake_peer)
    /// reads it lock-free thereafter.
    pub(crate) fn set_peer_wake(&self, wake: Arc<DeferredWake>) {
        let _ = self.peer_wake.set(wake);
    }

    /// Install the audio worker's data-arrival wake (idempotent — only the first
    /// set sticks). Called by the coord once the worker exists; the fire methods
    /// read it lock-free thereafter.
    pub(crate) fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>) {
        let _ = self.worker_wake.set(wake);
    }

    /// Wake the HLS peer's `poll_next` so it re-runs `reconcile_escape` against
    /// the just-flagged stalled slot. Called from the `on_slow` hook on the
    /// downloader thread (off-RT — `notify_now`'s cross-thread `notify_one` is
    /// allowed here). A no-op until the peer activates; the stored-permit
    /// semantics mean a wake delivered between the peer's polls is not lost.
    pub(crate) fn wake_peer(&self) {
        if let Some(wake) = self.peer_wake.get() {
            wake.notify_now();
        }
    }
}
