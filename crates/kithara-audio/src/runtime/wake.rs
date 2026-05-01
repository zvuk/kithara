//! Wake primitive for the scheduler.

use std::time::Duration;

use kithara_platform::{
    sync::{Condvar, Mutex},
    time::Instant,
};

/// Level-triggered wake for the scheduler thread.
pub(crate) struct SchedulerWake {
    condvar: Condvar,
    woken: Mutex<bool>,
}

impl SchedulerWake {
    /// Create a new wake in the "not woken" state.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            woken: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Block until woken or `timeout` elapses.
    ///
    /// Returns `true` if woken by [`wake`](Self::wake), `false` on timeout.
    /// Clears the flag on successful wake.
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
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

    /// Signal the scheduler to wake up. Safe to call from any thread.
    pub(crate) fn wake(&self) {
        *self.woken.lock_sync() = true;
        self.condvar.notify_one();
    }
}

impl Default for SchedulerWake {
    fn default() -> Self {
        Self::new()
    }
}
