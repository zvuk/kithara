#![forbid(unsafe_code)]

use std::sync::{Arc, OnceLock};

use kithara_platform::{
    thread::{JoinHandle, spawn_named},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::{FlushHub, FlushPolicy};

/// Slot for the background flush thread join handle.
///
/// `wasm_safe_thread::JoinHandle` is `!Sync` (carries `PhantomData<Cell<()>>`)
/// which would force the whole `Arc<FlushHub>` graph to be `!Sync` and break
/// the `Assets: Send + Sync` bound on `LeaseAssets`. So worker storage lives
/// only on native; wasm32 uses [`super::worker_stub::WorkerSlot`] instead.
pub(super) struct WorkerSlot {
    handle: OnceLock<JoinHandle<()>>,
}

impl WorkerSlot {
    pub(super) fn new() -> Self {
        Self {
            handle: OnceLock::new(),
        }
    }

    pub(super) fn is_started(&self) -> bool {
        self.handle.get().is_some()
    }

    fn start_with(&self, hub: Arc<FlushHub>) {
        self.handle
            .get_or_init(|| spawn_named("kithara-flush-hub", move || hub.run()));
    }
}

impl FlushHub {
    /// Spawn the background flush worker (idempotent). Subsequent
    /// `signal()` calls coalesce mutations through `policy.debounce`
    /// before flushing.
    pub fn start_worker(self: &Arc<Self>) {
        self.worker.start_with(Arc::clone(self));
    }

    /// Convenience: [`Self::new`] followed by [`Self::start_worker`].
    #[must_use]
    pub fn with_worker(cancel: CancellationToken, policy: FlushPolicy) -> Arc<Self> {
        let hub = Self::new(cancel, policy);
        hub.start_worker();
        hub
    }

    fn run(self: Arc<Self>) {
        loop {
            let mut guard = self.state.lock_sync();
            while !guard.pending {
                if self.cancel.is_cancelled() {
                    drop(guard);
                    let _g = self.flush_lock.lock_sync();
                    let _ = self.flush_dirty(false);
                    return;
                }
                let deadline = Instant::now() + self.policy.poll_interval;
                let (next, _) = self.cv.wait_sync_timeout(guard, deadline);
                guard = next;
            }
            drop(guard);

            let debounce_deadline = Instant::now() + self.policy.debounce;
            loop {
                let guard = self.state.lock_sync();
                if guard.op_count >= self.policy.force_every_n_ops.get() {
                    break;
                }
                if self.cancel.is_cancelled() {
                    drop(guard);
                    let _g = self.flush_lock.lock_sync();
                    let _ = self.flush_dirty(false);
                    return;
                }
                if Instant::now() >= debounce_deadline {
                    break;
                }
                let (_next, _) = self.cv.wait_sync_timeout(guard, debounce_deadline);
            }

            {
                let mut guard = self.state.lock_sync();
                guard.pending = false;
                guard.op_count = 0;
            }
            let _g = self.flush_lock.lock_sync();
            let _ = self.flush_dirty(false);
        }
    }
}
