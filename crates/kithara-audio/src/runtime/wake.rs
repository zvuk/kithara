use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use kithara_platform::{
    thread::{self, Thread, park_timeout},
    time::{Duration, Instant},
};

/// Level-triggered wake for the scheduler thread.
///
/// The wake side is wait-free: it sets an atomic flag and unparks the
/// scheduler thread, never taking a lock. This matters because the
/// real-time audio thread wakes the worker after consuming a chunk
/// (`Audio::recv_outcome` → `wake_worker`), and a lock there (e.g. a
/// condvar's internal mutex) is forbidden on the audio thread.
#[derive(Default)]
pub(crate) struct SchedulerWake {
    woken: AtomicBool,
    /// The single scheduler thread, registered on its first wait. Read
    /// lock-free by wakers via [`OnceLock::get`].
    waiter: OnceLock<Thread>,
}

impl SchedulerWake {
    /// Block until woken or `timeout` elapses.
    ///
    /// Returns `true` if woken by [`wake`](Self::wake), `false` on timeout.
    /// Clears the flag on successful wake. Called only from the scheduler
    /// thread, which registers itself here on the first call so wakers can
    /// unpark it.
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.waiter.get().is_none() {
            let _ = self.waiter.set(thread::current());
        }
        let deadline = Instant::now() + timeout;
        loop {
            if self.woken.swap(false, Ordering::Acquire) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            park_timeout(deadline - now);
            if self.woken.swap(false, Ordering::Acquire) {
                return true;
            }
        }
    }

    /// Signal the scheduler to wake up. Wait-free and safe to call from any
    /// thread, including the real-time audio thread.
    pub(crate) fn wake(&self) {
        self.woken.store(true, Ordering::Release);
        if let Some(waiter) = self.waiter.get() {
            waiter.unpark();
        }
    }
}
