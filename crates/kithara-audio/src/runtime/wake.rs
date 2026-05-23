use kithara_platform::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

/// Level-triggered wake for the scheduler thread.
#[derive(Default)]
pub(crate) struct SchedulerWake {
    condvar: Condvar,
    woken: Mutex<bool>,
}

impl SchedulerWake {
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
            if result.did_time_out() {
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
