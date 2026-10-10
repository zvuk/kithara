use kithara_platform::sync::Arc;

/// Playback adapter for the base worker wake capability.
#[derive(Clone)]
pub struct StreamWake(kithara_worker::Wake);

impl StreamWake {
    #[must_use]
    pub const fn new(wake: kithara_worker::Wake) -> Self {
        Self(wake)
    }
}

impl kithara_stream::WorkerWake for StreamWake {
    delegate::delegate! {
        to self.0 {
            fn wake(&self);
            fn defer(&self);
        }
    }
}

impl std::task::Wake for StreamWake {
    fn wake(self: Arc<Self>) {
        kithara_stream::WorkerWake::wake(&*self);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        kithara_stream::WorkerWake::wake(&**self);
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::{
        sync::{Arc, ThreadGate, WaitGate, mpsc},
        thread::{self, spawn},
        time::Duration,
    };
    use kithara_test_utils::kithara;

    #[kithara::test(flash(false))]
    fn cross_thread_wake_after_snapshot_is_observed() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let wake = Arc::new(ThreadGate::default());
            let worker_wake = Arc::clone(&wake);
            let (snapshot_tx, snapshot_rx) = mpsc::channel();

            let join = spawn(move || {
                let since = worker_wake.current();
                snapshot_tx.send(()).expect("report wake snapshot");
                worker_wake.wait_timeout(since, Duration::from_secs(1))
            });

            snapshot_rx.recv().expect("receive wake snapshot");
            wake.signal();
            assert!(join.join().expect("wake test thread"));
        }
    }

    #[kithara::test]
    fn wake_releases_waiter_before_timeout() {
        let wake = Arc::new(ThreadGate::default());
        let signaller = Arc::clone(&wake);
        let since = wake.current();

        let join = spawn(move || {
            thread::sleep(Duration::from_millis(5));
            signaller.signal();
        });

        assert!(wake.wait_timeout(since, Duration::from_secs(1)));
        join.join().expect("wake signaller thread");
    }

    #[kithara::test]
    fn wake_between_snapshot_and_wait_is_not_lost() {
        let wake = ThreadGate::default();
        let since = wake.current();
        wake.signal();

        assert!(wake.wait_timeout(since, Duration::ZERO));
    }
}
