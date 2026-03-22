use kithara_platform::{
    sync::Mutex,
    thread::{self, Thread},
};

pub(crate) struct ThreadWake {
    waiter: Mutex<Option<Thread>>,
}

impl ThreadWake {
    pub(crate) fn new() -> Self {
        Self {
            waiter: Mutex::new(None),
        }
    }

    pub(crate) fn register_current(&self) {
        *self.waiter.lock_sync() = Some(thread::current());
    }

    pub(crate) fn wake(&self) {
        let waiter = self.waiter.lock_sync().as_ref().cloned();
        if let Some(waiter) = waiter {
            waiter.unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use kithara_platform::thread::{park_timeout, sleep, spawn};
    use kithara_test_utils::kithara;

    use super::ThreadWake;

    #[kithara::test]
    fn wake_unparks_registered_thread() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let wake = Arc::new(ThreadWake::new());
            let done = Arc::new(AtomicBool::new(false));
            let worker_wake = Arc::clone(&wake);
            let worker_done = Arc::clone(&done);

            let join = spawn(move || {
                worker_wake.register_current();
                park_timeout(Duration::from_secs(1));
                worker_done.store(true, Ordering::Release);
            });

            sleep(Duration::from_millis(10));
            wake.wake();
            join.join().expect("wake test thread");
            assert!(done.load(Ordering::Acquire));
        }
    }
}
