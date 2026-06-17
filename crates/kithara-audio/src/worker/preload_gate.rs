use std::sync::atomic::{AtomicBool, Ordering};

use kithara_platform::time::{Duration, sleep};
use kithara_test_utils::kithara;

/// Decoupled startup gate between the worker thread and the async app task.
///
/// The worker signals readiness with a single `Release` store on
/// [`signal`](PreloadGate::signal); it never takes a lock or syscall, so the
/// worker preload path stays real-time-safe. The async awaiter
/// ([`wait`](PreloadGate::wait)) is a plain tokio task that polls `ready` and
/// re-arms its own runtime timer while the gate is still closed — the worker
/// does not drive the wakeup.
///
/// The contract (signal sites, rearm on seek) lives in
/// `crates/kithara-audio/README.md`.
#[derive(Default)]
pub struct PreloadGate {
    ready: AtomicBool,
}

impl PreloadGate {
    /// Coarse re-poll interval for the closed gate. Preload is a one-time
    /// startup wait before first audio, so a few-ms cadence is below the
    /// perceptible budget while keeping the worker store-only.
    const POLL_INTERVAL: Duration = Duration::from_millis(2);

    /// `true` once [`signal`](PreloadGate::signal) has fired for the current
    /// epoch.
    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Re-close the gate so a fresh [`wait`](PreloadGate::wait) blocks again.
    /// Called on the worker thread when a seek resets the decoder runtime.
    pub(crate) fn rearm(&self) {
        self.ready.store(false, Ordering::Release);
    }

    /// Mark the gate open. Called on the worker thread from every preload
    /// terminal site (progress threshold, EOF, Failed, cancel). Idempotent
    /// and lock-free: a single `Release` store.
    pub(crate) fn signal(&self) {
        self.ready.store(true, Ordering::Release);
    }

    /// Await the gate opening. Resolves once a worker `signal()` is observed;
    /// until then the awaiter parks on its own runtime timer. A `signal()`
    /// landing during a sleep is observed on the next poll (worst-case
    /// latency one [`POLL_INTERVAL`](Self::POLL_INTERVAL)).
    #[kithara::flash(true)]
    pub async fn wait(&self) {
        while !self.is_ready() {
            sleep(Self::POLL_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_platform::{thread, time::Duration};
    use kithara_test_utils::kithara;

    use super::PreloadGate;

    #[kithara::test(tokio)]
    async fn wait_resolves_after_signal() {
        let gate = Arc::new(PreloadGate::default());
        assert!(!gate.is_ready());

        let signaller = Arc::clone(&gate);
        // spawn_named: the signaller must be engine-visible under flash — a bare
        // spawn sleeps in REAL time while the waiter's poll loop and the timeout
        // run on the virtual clock, so the virtual 1s can elapse before the real
        // 5ms signal lands (mixed-clock race).
        let join = thread::spawn_named("preload-signal", move || {
            thread::sleep(Duration::from_millis(5));
            signaller.signal();
        });

        time::timeout(Duration::from_secs(1), gate.wait())
            .await
            .expect("signal must open the gate");
        assert!(gate.is_ready());
        join.join().expect("signaller thread");
    }

    #[kithara::test(tokio)]
    async fn rearm_reblocks_a_fresh_wait() {
        let gate = Arc::new(PreloadGate::default());
        gate.signal();
        gate.wait().await;

        gate.rearm();
        assert!(!gate.is_ready());

        let re_signaller = Arc::clone(&gate);
        // spawn_named for the same mixed-clock reason as wait_resolves_after_signal.
        let join = thread::spawn_named("preload-resignal", move || {
            thread::sleep(Duration::from_millis(5));
            re_signaller.signal();
        });

        time::timeout(Duration::from_secs(1), gate.wait())
            .await
            .expect("re-armed gate must reopen on the next signal");
        join.join().expect("re-signaller thread");
    }
}
