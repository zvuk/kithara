use kithara_platform::{
    sync::Mutex,
    thread::{self, Thread},
};

#[derive(Default)]
pub(crate) struct ThreadWake {
    waiter: Mutex<Option<Thread>>,
}

impl ThreadWake {
    pub(crate) fn register_current(&self) {
        *self.waiter.lock() = Some(thread::current());
    }
}

impl crate::runtime::WakeSignal for ThreadWake {
    fn wake(&self) {
        let waiter = self.waiter.lock().as_ref().cloned();
        if let Some(waiter) = waiter {
            thread::unpark(&waiter);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use kithara_platform::{
        sync::Arc,
        thread::{self, park_timeout, spawn},
        time::Duration,
    };
    use kithara_test_utils::kithara;

    use super::ThreadWake;
    use crate::runtime::WakeSignal;

    #[kithara::test]
    fn wake_unparks_registered_thread() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let wake = Arc::new(ThreadWake::default());
            let done = Arc::new(AtomicBool::new(false));
            let worker_wake = Arc::clone(&wake);
            let worker_done = Arc::clone(&done);

            let join = spawn(move || {
                worker_wake.register_current();
                park_timeout(Duration::from_secs(1));
                worker_done.store(true, Ordering::Release);
            });

            thread::sleep(Duration::from_millis(10)); // M5: real pacing, replace with teardown signal
            wake.wake();
            join.join().expect("wake test thread");
            assert!(done.load(Ordering::Acquire));
        }
    }
}
