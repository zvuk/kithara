//! Wake primitive for the scheduler.

use std::time::Duration;

use kithara_platform::{
    sync::{Condvar, Mutex},
    time::Instant,
};

/// Level-triggered wake for the scheduler thread.
pub struct SchedulerWake {
    woken: Mutex<bool>,
    condvar: Condvar,
}

impl SchedulerWake {
    /// Create a new wake in the "not woken" state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            woken: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Signal the scheduler to wake up. Safe to call from any thread.
    pub fn wake(&self) {
        *self.woken.lock_sync() = true;
        self.condvar.notify_one();
    }

    /// Block until woken or `timeout` elapses.
    ///
    /// Returns `true` if woken by [`wake`](Self::wake), `false` on timeout.
    /// Clears the flag on successful wake.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
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
}

impl Default for SchedulerWake {
    fn default() -> Self {
        Self::new()
    }
}
