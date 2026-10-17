use std::sync::OnceLock;

use kithara_platform::{
    CancelToken,
    sync::{Arc, CondvarGate, WaitGate},
    time::Duration,
};
use kithara_storage::WaitOutcome;
use kithara_stream::{DeferredWake, SourceError, StreamError, StreamResult, WorkerWake};
use kithara_test_utils::kithara;

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

#[derive(Clone)]
pub(crate) struct SizeSignal {
    /// Direct off-RT peer-poll wake. Unlike `peer_wake`, this invokes the
    /// downloader's registered task waker without waiting for a forwarding
    /// micro-task to be scheduled.
    peer_poll_wake: Arc<OnceLock<Arc<dyn WorkerWake>>>,
    /// Deferred peer wake used by RT callers, which arm it for the scheduler
    /// shell to flush without invoking a task waker on the produce core.
    peer_wake: Arc<OnceLock<Arc<DeferredWake>>>,
    ready: Arc<CondvarGate<u64>>,
    worker_wake: WorkerWakeCell,
}

impl SizeSignal {
    /// Re-aim heartbeat for the off-RT blocking wait. The wait wakes immediately
    /// on any readiness signal (the fact of a write/commit/fence/seek) —
    /// event-driven. This interval bounds only the *quiet* case: if no signal
    /// arrives within it, the peer may be mis-aimed after a seek (it fetched,
    /// went idle, and the range the reader now wants is outside its prefetch
    /// window). The next internal probe re-asserts the peer's aim while the
    /// same parser operation remains parked until its range resolves.
    const READER_REAIM_INTERVAL: Duration = Duration::from_millis(25);

    /// Construct from a fresh readiness gate and an empty worker-wake cell. Built
    /// once in `Hls::create` and cloned down into every consumer.
    pub(crate) fn new(ready: Arc<CondvarGate<u64>>, worker_wake: WorkerWakeCell) -> Self {
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
    pub(crate) fn ready_gate(&self) -> Arc<CondvarGate<u64>> {
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

    /// Off-RT blocking wait: park on the readiness gate until the caller's probe
    /// resolves (`Ready`/`Eof`/`Interrupted`) or returns a terminal error.
    /// Event-driven — every transition that can flip the probe (segment
    /// write/commit/fail, seek reset, cancel) `signal`s the gate. The
    /// pre-probe [`current`](WaitGate::current) snapshot + park-only-
    /// if-unchanged is a seqlock guard closing the lost-wakeup window even
    /// though the probe predicate and the gate sit under different locks
    /// (mirrors `kithara-storage` `wait_range_inner`). A genuine wedge (no
    /// signal at all) trips the hang watchdog rather than parking forever.
    #[kithara::hang_watchdog]
    pub(crate) fn wait_range_blocking(
        &self,
        cancel: &CancelToken,
        reader_cancel: Option<&kithara_stream::ReaderControl>,
        mut probe: impl FnMut() -> StreamResult<WaitOutcome>,
    ) -> StreamResult<WaitOutcome> {
        let _cancel_wake = {
            let ready = self.ready_gate();
            cancel.on_cancel(move || ready.signal())
        };
        let _reader_cancel_wake = reader_cancel.map(|cancel| {
            let ready = self.ready_gate();
            cancel.cancel_token().on_cancel(move || ready.signal())
        });
        loop {
            hang_tick!();
            if cancel.is_cancelled()
                || reader_cancel.is_some_and(kithara_stream::ReaderControl::is_cancelled)
            {
                return Ok(WaitOutcome::Interrupted);
            }
            let since = self.current();
            match probe() {
                Ok(WaitOutcome::Ready) => {
                    hang_reset!();
                    if let Some(control) = reader_cancel {
                        control.clear_pending();
                    }
                    return Ok(WaitOutcome::Ready);
                }
                Ok(WaitOutcome::Eof) => {
                    hang_reset!();
                    return Ok(WaitOutcome::Eof);
                }
                Ok(WaitOutcome::Interrupted) => return Ok(WaitOutcome::Interrupted),
                Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                    if let Some(control) = reader_cancel {
                        control.set_pending(kithara_stream::PendingReason::NotReady(
                            kithara_stream::NotReadyCause::WaitBudgetExhausted,
                        ));
                    }
                    self.wake_peer();
                }
                Err(e) => return Err(e),
            }
            self.wait_timeout(since, Self::READER_REAIM_INTERVAL);
        }
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

#[cfg(test)]
mod tests {
    use kithara_platform::{
        sync::{
            Arc, CondvarGate, WaitGate,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use kithara_storage::WaitOutcome;
    use kithara_stream::{DeferredWake, SourceError, StreamError, WorkerWake};
    use kithara_test_utils::{cancel_token, kithara};

    use super::SizeSignal;

    struct ReadyPeer {
        ready: Arc<AtomicBool>,
        gate: Arc<CondvarGate<u64>>,
        wakes: Arc<AtomicUsize>,
    }

    impl WorkerWake for ReadyPeer {
        fn defer(&self) {
            panic!("a blocking reader must wake its peer immediately");
        }

        fn wake(&self) {
            self.wakes.fetch_add(1, Ordering::Relaxed);
            self.ready.store(true, Ordering::Release);
            self.gate.signal();
        }
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
    fn blocking_wait_wakes_the_peer_before_parking_on_unready_bytes() {
        let gate = Arc::new(CondvarGate::default());
        let ready = Arc::new(AtomicBool::new(false));
        let wakes = Arc::new(AtomicUsize::new(0));
        let signal = SizeSignal::new(Arc::clone(&gate), Arc::default());
        signal.set_peer_wake(
            Arc::new(DeferredWake::default()),
            Arc::new(ReadyPeer {
                ready: Arc::clone(&ready),
                gate,
                wakes: Arc::clone(&wakes),
            }),
        );
        let cancel = cancel_token();
        let mut probes = 0;
        let result = signal.wait_range_blocking(&cancel, None, || {
            probes += 1;
            if ready.load(Ordering::Acquire) {
                Ok(WaitOutcome::Ready)
            } else {
                Err(StreamError::Source(SourceError::WaitBudgetExceeded))
            }
        });

        assert!(matches!(result, Ok(WaitOutcome::Ready)), "{result:?}");
        assert_eq!(
            probes, 2,
            "the actual wait rechecks the peer's readiness signal"
        );
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            1,
            "one unready probe wakes the peer once"
        );
    }
}
