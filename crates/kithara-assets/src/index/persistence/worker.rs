#![forbid(unsafe_code)]

use std::sync::{OnceLock, Weak};

use kithara_platform::{
    CancelToken,
    sync::Arc,
    thread::{JoinHandle, spawn_named},
};

use super::{FlushHub, FlushPolicy, flush::HubWait};

/// Slot for the background flush thread join handle.
///
/// `wasm_safe_thread::JoinHandle` is `!Sync` (carries `PhantomData<Cell<()>>`)
/// which would force the whole `Arc<FlushHub>` graph to be `!Sync` and break
/// the `Assets: Send + Sync` bound on `LeaseAssets`. So worker storage lives
/// only on native; wasm32 uses the sibling stub instead.
#[derive(Default)]
pub(super) struct WorkerSlot {
    handle: OnceLock<JoinHandle<()>>,
}

impl WorkerSlot {
    pub(super) fn is_started(&self) -> bool {
        self.handle.get().is_some()
    }

    /// Spawn once, retaining the hub only while flushing. Final hub destruction
    /// cancels and detaches the worker; its remaining weak reference cannot
    /// access sources. Dropping the last external handle can leave an in-flight
    /// flush owning the hub until that flush completes.
    pub(super) fn start_with(&self, hub: &Arc<FlushHub>) {
        self.handle.get_or_init(|| {
            let weak = Arc::downgrade(hub);
            let wait = Arc::clone(&hub.wait);
            let cancel = hub.cancel.clone();
            let policy = hub.policy.clone();
            spawn_named("kithara-flush-hub", move || {
                let cancel_wait = Arc::clone(&wait);
                let _cancel_wake = cancel.on_cancel(move || {
                    let _guard = cancel_wait.state.lock();
                    cancel_wait.cv.notify_all();
                });
                run(&weak, &wait, &cancel, &policy);
            })
        });
    }
}

/// Background flush loop. Coalesces a burst of `signal()` calls through
/// `policy.debounce` (bypassed once `op_count` reaches
/// `force_every_n_ops`) before draining dirty sources. Exits on cancel
/// after a final flush, or when the hub has been dropped.
///
/// The wait is event-driven with no bounded poll: `signal()` sets pending state and notifies, and
/// shutdown both cancels and notifies, so a periodic re-check is unnecessary.
fn run(weak: &Weak<FlushHub>, wait: &HubWait, cancel: &CancelToken, policy: &FlushPolicy) {
    loop {
        let mut guard = wait.state.lock();
        while !guard.pending {
            if cancel.is_cancelled() {
                drop(guard);
                final_flush(weak);
                return;
            }
            guard = wait.cv.wait(guard);
        }
        drop(guard);

        let debounce_deadline = wait.cv.instant() + policy.debounce;
        loop {
            let guard = wait.state.lock();
            if guard.op_count >= policy.force_every_n_ops.get() {
                break;
            }
            if cancel.is_cancelled() {
                drop(guard);
                final_flush(weak);
                return;
            }
            if wait.cv.instant() >= debounce_deadline {
                break;
            }
            let _ = wait.cv.wait_timeout(guard, debounce_deadline);
        }

        {
            let mut guard = wait.state.lock();
            guard.pending = false;
            guard.op_count = 0;
        }
        let Some(hub) = weak.upgrade() else {
            return;
        };
        let _g = hub.flush_lock.lock();
        let _ = hub.flush_dirty(false);
    }
}

/// Final best-effort flush on shutdown. No-op if the hub is already
/// gone (its sources were dropped alongside it).
fn final_flush(weak: &Weak<FlushHub>) {
    let Some(hub) = weak.upgrade() else {
        return;
    };
    let _g = hub.flush_lock.lock();
    let _ = hub.flush_dirty(false);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use kithara_platform::sync::{Mutex, mpsc};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{error::AssetsResult, index::persistence::flush::Flushable};

    #[kithara::test]
    fn final_hub_destruction_prevents_worker_source_operations() {
        struct Source {
            dirty: AtomicBool,
            flushes: AtomicUsize,
        }
        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }
            fn flush(&self) -> AssetsResult<()> {
                self.flushes.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            fn name(&self) -> &'static str {
                "final-owner"
            }
        }

        let hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
        let source = Arc::new(Source {
            dirty: AtomicBool::new(true),
            flushes: AtomicUsize::new(0),
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);
        let weak = Arc::downgrade(&hub);
        let mut hub = Arc::try_unwrap(hub).expect("idle worker holds only a weak hub");
        let worker = hub.worker.handle.take().expect("registered worker exists");

        drop(hub);
        worker.join().expect("cancelled worker exits");

        assert!(weak.upgrade().is_none());
        assert_eq!(source.flushes.load(Ordering::Acquire), 0);
        assert!(source.dirty.load(Ordering::Acquire));
    }

    #[kithara::test]
    fn worker_can_destroy_the_hub_after_releasing_its_last_external_owner() {
        struct Source {
            dirty: AtomicBool,
            owner: Mutex<Option<Arc<FlushHub>>>,
            release: Mutex<mpsc::Receiver<()>>,
            flushed: mpsc::Sender<bool>,
        }
        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }
            fn flush(&self) -> AssetsResult<()> {
                self.release.lock().recv().expect("release in-flight flush");
                let hub = self.owner.lock().take().expect("last external hub owner");
                let weak = Arc::downgrade(&hub);
                drop(hub);
                self.flushed
                    .send(weak.upgrade().is_some())
                    .expect("report worker ownership");
                Ok(())
            }
            fn name(&self) -> &'static str {
                "worker-owner"
            }
        }

        let cancel = CancelToken::never();
        let hub = FlushHub::new(cancel.clone(), FlushPolicy::default());
        let weak = Arc::downgrade(&hub);
        let (release, released) = mpsc::channel();
        let (flushed, completion) = mpsc::channel();
        let (destroyed, destruction) = mpsc::channel();
        let _on_cancel = cancel.on_cancel(move || destroyed.send(()).unwrap());
        let source = Arc::new(Source {
            dirty: AtomicBool::new(true),
            owner: Mutex::new(Some(Arc::clone(&hub))),
            release: Mutex::new(released),
            flushed,
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);
        hub.signal();
        drop(hub);
        release
            .send(())
            .expect("allow worker to release external owner");

        assert!(completion.recv().expect("worker completes source flush"));
        destruction
            .recv()
            .expect("worker destroys the hub without self-joining");
        assert!(weak.upgrade().is_none());
    }
}
