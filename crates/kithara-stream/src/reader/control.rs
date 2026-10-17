use std::{fmt, sync::OnceLock};

use kithara_platform::{
    CancelScope, CancelToken,
    sync::{Arc, CondvarGate},
};

use crate::{PendingReason, WorkerWake};

#[derive(Default)]
struct Readiness {
    revision: u64,
    pending: Option<PendingReason>,
}

struct ReaderState {
    cancel: CancelScope,
    readiness: CondvarGate<Readiness>,
    native: OnceLock<()>,
    wake: OnceLock<Arc<dyn WorkerWake>>,
}

/// Cancellation and readiness of one opened decoder input.
/// Nonblocking custom inputs signal readiness after publishing new bytes.
#[derive(Clone)]
#[non_exhaustive]
pub struct ReaderControl {
    state: Arc<ReaderState>,
}

impl fmt::Debug for ReaderControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReaderControl")
            .field("cancelled", &self.is_cancelled())
            .field("blocking", &self.is_blocking())
            .finish_non_exhaustive()
    }
}

impl ReaderControl {
    /// Derive an independent reader lifetime from its source owner.
    #[must_use]
    pub fn new(parent: Option<CancelToken>) -> Self {
        Self {
            state: Arc::new(ReaderState {
                cancel: CancelScope::new(parent),
                readiness: CondvarGate::default(),
                native: OnceLock::new(),
                wake: OnceLock::new(),
            }),
        }
    }

    /// Transfer this input to a persistent native parser before its first operation.
    pub fn require_blocking(&self) {
        let _ = self.state.native.set(());
    }

    /// Native parser input always waits off-core, independently of caller preparation policy.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.state.native.get().is_some()
    }

    /// Bind the owning audio lane's completion wake before starting parser work.
    pub fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>) {
        let _ = self.state.wake.set(wake);
    }

    /// Notify the owning lane after parser progress without invoking it on the checked core.
    pub fn wake_worker(&self) {
        if let Some(wake) = self.state.wake.get() {
            wake.wake();
        }
    }

    delegate::delegate! {
        to self.state.cancel {
            /// Cancellation observed by this reader's actual I/O waits.
            #[must_use]
            #[call(token)]
            pub fn cancel_token(&self) -> CancelToken;
            /// Whether the reader has been retired.
            #[must_use]
            pub fn is_cancelled(&self) -> bool;
        }
        to self.state.readiness {
            /// Observe the readiness edge before attempting a read.
            #[must_use]
            #[expr($.revision)]
            #[call(lock)]
            pub fn revision(&self) -> u64;
            /// Exact availability condition encountered by an operation still in flight.
            #[must_use]
            #[expr($.pending)]
            #[call(lock)]
            pub fn pending_reason(&self) -> Option<PendingReason>;
        }
    }

    /// Retire only this reader, leaving its source and siblings live.
    pub fn cancel(&self) {
        self.state.cancel.cancel();
        self.notify();
    }

    /// Publish availability after bytes or a terminal input result changed.
    pub fn notify(&self) {
        {
            let mut readiness = self.state.readiness.lock();
            readiness.revision = readiness.revision.wrapping_add(1);
            readiness.pending = None;
        }
        self.state.readiness.notify_all();
    }

    /// Retain a source's typed condition without ending its parser operation.
    pub fn set_pending(&self, reason: PendingReason) {
        self.state.readiness.lock().pending = Some(reason);
        self.wake_worker();
    }

    /// Clear a condition after source progress without manufacturing a readiness edge.
    pub fn clear_pending(&self) {
        self.state.readiness.lock().pending = None;
    }

    /// Wait for a source publication or this reader's cancellation.
    /// Returns false when the reader was cancelled.
    #[must_use]
    pub fn wait(&self, revision: u64) -> bool {
        let control = self.clone();
        let _cancel_wake = self
            .state
            .cancel
            .token()
            .on_cancel(move || control.notify());
        let mut readiness = self.state.readiness.lock();
        while readiness.revision == revision && !self.is_cancelled() {
            readiness = self.state.readiness.wait(readiness);
        }
        drop(readiness);
        !self.is_cancelled()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn cancellation_at_park_publishes_a_guarded_readiness_edge() {
        for from_parent in [false, true] {
            let parent = CancelScope::new(None);
            let control = ReaderControl::new(Some(parent.token()));
            let revision = control.revision();
            let (cancelled_tx, cancelled_rx) = mpsc::channel();
            let _observed_cancel = control.cancel_token().on_cancel(move || {
                cancelled_tx.send(()).expect("report reader cancellation");
            });
            let mut readiness = control.state.readiness.lock();
            assert_eq!(readiness.revision, revision);
            assert!(!control.is_cancelled());

            let waiting_control = control.clone();
            let reader = thread::spawn_named("reader-control-wait", move || {
                waiting_control.wait(revision)
            });
            let cancelling_control = control.clone();
            let cancelling_parent = parent.token();
            let canceller = thread::spawn_named("reader-control-cancel", move || {
                if from_parent {
                    cancelling_parent.cancel();
                } else {
                    cancelling_control.cancel();
                }
            });
            cancelled_rx
                .recv_timeout(Instant::now() + Duration::from_secs(1))
                .expect("cancellation lands after the live predicate, before parking");
            assert!(control.is_cancelled());

            let deadline = Instant::now() + Duration::from_secs(1);
            while readiness.revision == revision && Instant::now() < deadline {
                readiness = control.state.readiness.wait_until(readiness, deadline);
            }
            let published = readiness.revision != revision;
            drop(readiness);

            canceller.join().expect("cancelling owner");
            assert!(!reader.join().expect("cancelled reader"));
            assert!(
                published,
                "the cancellation wake must publish under the readiness lock"
            );
            assert_eq!(parent.is_cancelled(), from_parent);
        }
    }
}
