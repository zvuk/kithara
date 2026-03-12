//! Wake primitive for the shared audio worker.
//!
//! [`WorkerWake`] wraps a `Mutex<bool>` + `Condvar` pair to provide a
//! level-triggered wake mechanism. The worker thread calls
//! [`wait_timeout`](WorkerWake::wait_timeout) to sleep until woken or
//! a deadline expires. Any thread (tokio bridge, audio callback,
//! ringbuf consumer) can call [`wake`](WorkerWake::wake) to unblock it.

use std::time::Duration;

use kithara_platform::sync::{Condvar, Mutex};

/// Level-triggered wake for the shared worker thread.
///
/// Multiple [`wake`](Self::wake) calls before a single
/// [`wait_timeout`](Self::wait_timeout) coalesce into one wakeup.
pub(crate) struct WorkerWake {
    woken: Mutex<bool>,
    condvar: Condvar,
}

impl WorkerWake {
    /// Create a new wake in the "not woken" state.
    pub(crate) fn new() -> Self {
        Self {
            woken: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Signal the worker to wake up. Safe to call from any thread.
    pub(crate) fn wake(&self) {
        *self.woken.lock_sync() = true;
        self.condvar.notify_one();
    }

    /// Block until woken or `timeout` elapses.
    ///
    /// Returns `true` if woken by [`wake`](Self::wake), `false` on timeout.
    /// Clears the flag on successful wake.
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline = kithara_platform::time::Instant::now() + timeout;
        let mut guard = self.woken.lock_sync();
        loop {
            if *guard {
                *guard = false;
                return true;
            }
            let (new_guard, result) = self.condvar.wait_sync_timeout(guard, deadline);
            guard = new_guard;
            if *guard {
                *guard = false;
                drop(guard);
                return true;
            }
            if result.timed_out() {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn wake_then_wait_returns_immediately() {
        let w = WorkerWake::new();
        w.wake();
        assert!(w.wait_timeout(Duration::from_secs(5)));
    }

    #[kithara::test]
    fn wait_without_wake_times_out() {
        let w = WorkerWake::new();
        let start = kithara_platform::time::Instant::now();
        let woken = w.wait_timeout(Duration::from_millis(10));
        assert!(!woken);
        assert!(start.elapsed() >= Duration::from_millis(5));
    }

    #[kithara::test]
    fn multiple_wakes_coalesce() {
        let w = WorkerWake::new();
        w.wake();
        w.wake();
        w.wake();
        assert!(w.wait_timeout(Duration::from_millis(1)));
        assert!(!w.wait_timeout(Duration::from_millis(1)));
    }

    #[kithara::test]
    fn wake_from_another_thread() {
        let w = std::sync::Arc::new(WorkerWake::new());
        let w2 = std::sync::Arc::clone(&w);
        kithara_platform::thread::spawn(move || {
            kithara_platform::thread::sleep(Duration::from_millis(5));
            w2.wake();
        });
        assert!(w.wait_timeout(Duration::from_secs(5)));
    }
}
