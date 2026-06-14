use std::sync::atomic::{AtomicU64, Ordering};

use kithara_platform::{
    sync::{ThreadGate, WaitGate},
    time::Duration,
};

/// Level-triggered wake for the scheduler thread — a thin adapter over the
/// shared [`ThreadGate`] (RT-safe lock-free wake + `park_timeout` backstop).
///
/// The wake side stays wait-free (atomic bump + `unpark`, no lock): the RT
/// audio thread wakes the worker after consuming a chunk
/// (`Audio::recv_outcome` → `wake_worker`), where a condvar's internal mutex is
/// forbidden. `seen` carries the level/consume semantics the scheduler loop
/// relies on — it is snapshotted at the END of each wait (i.e. before the next
/// produce pass), so a `wake` that lands during that pass advances the gate
/// counter past it and the following `wait_timeout` returns at once: no missed
/// wake, no per-chunk latency.
#[derive(Default)]
pub(crate) struct SchedulerWake {
    gate: ThreadGate,
    /// Gate counter consumed as of the previous wait (scheduler-thread only).
    seen: AtomicU64,
}

impl SchedulerWake {
    /// Block until [`wake`](Self::wake) fires or `timeout` elapses. Returns
    /// `true` if woken. Called only from the scheduler thread (single waiter).
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        let since = self.seen.load(Ordering::Relaxed);
        let woken = self.gate.wait_timeout(since, timeout);
        self.seen.store(self.gate.current(), Ordering::Relaxed);
        woken
    }

    /// Signal the scheduler to wake up. Wait-free and safe to call from any
    /// thread, including the real-time audio thread.
    pub(crate) fn wake(&self) {
        self.gate.signal();
    }
}
