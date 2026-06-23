use std::sync::{Arc, OnceLock};

use kithara_platform::{
    sync::{CondvarGate, WaitGate},
    time::Duration,
};
use kithara_stream::WorkerWake;

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
    /// Shared readiness gate. Every transition that can flip a blocked reader's
    /// `wait_range` predicate (segment write/commit/fail, fence raise/clear,
    /// seek reset, cancel) signals it; the off-RT `wait_range(_, None)` parks on
    /// it instead of polling a wall-clock timer. See `CONTEXT.md`
    /// "Event-driven read wait".
    ready: Arc<CondvarGate<u64>>,
    /// Late-bound audio-worker wake. Fired alongside `ready` on the two
    /// downloader write/settle sites (NOT the coord's RT-reachable fence/seek
    /// signals), so the RT decoder's worker re-ticks on data arrival rather than
    /// on its 10 ms scheduler poll.
    worker_wake: WorkerWakeCell,
}

impl SizeSignal {
    /// Construct from a fresh readiness gate and an empty worker-wake cell. Built
    /// once in `Hls::create` and cloned down into every consumer.
    pub(crate) fn new(ready: Arc<CondvarGate<u64>>, worker_wake: WorkerWakeCell) -> Self {
        Self { ready, worker_wake }
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

    /// Signal only the gate. Used at the coord's RT-reachable transitions
    /// (fence clear, seek reset, cancel waker) that flip a blocked reader's
    /// predicate without producing new bytes — the audio worker re-discovers the
    /// state on its next scheduler poll, so it is deliberately not re-ticked.
    pub(crate) fn fire_ready_only(&self) {
        self.ready.signal();
    }

    /// Install the audio worker's data-arrival wake (idempotent — only the first
    /// set sticks). Called by the coord once the worker exists; the fire methods
    /// read it lock-free thereafter.
    pub(crate) fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>) {
        let _ = self.worker_wake.set(wake);
    }

    /// Re-vend the underlying readiness gate. Used by the cancel waker, which
    /// must capture a hard-`Send + Sync` handle in its `on_cancel` closure.
    pub(crate) fn ready_gate(&self) -> Arc<CondvarGate<u64>> {
        Arc::clone(&self.ready)
    }

    /// Pre-park snapshot of the gate generation (seqlock guard for the off-RT
    /// `wait_range` loop). Pass-through to [`CondvarGate::current`].
    pub(crate) fn current(&self) -> u64 {
        self.ready.current()
    }

    /// Park on the gate until its generation advances past `since` or `timeout`
    /// elapses. Returns `true` on a signal, `false` on timeout. Pass-through to
    /// [`CondvarGate::wait_timeout`].
    pub(crate) fn wait_timeout(&self, since: u64, timeout: Duration) -> bool {
        self.ready.wait_timeout(since, timeout)
    }
}
