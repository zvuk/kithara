#![forbid(unsafe_code)]

use std::sync::{Arc, OnceLock, Weak};

use kithara_platform::{
    thread::{JoinHandle, spawn_named},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::{FlushHub, FlushPolicy, core::HubWait};

/// Slot for the background flush thread join handle.
///
/// `wasm_safe_thread::JoinHandle` is `!Sync` (carries `PhantomData<Cell<()>>`)
/// which would force the whole `Arc<FlushHub>` graph to be `!Sync` and break
/// the `Assets: Send + Sync` bound on `LeaseAssets`. So worker storage lives
/// only on native; wasm32 uses [`super::worker_stub::WorkerSlot`] instead.
#[derive(Default)]
pub(super) struct WorkerSlot {
    handle: OnceLock<JoinHandle<()>>,
}

impl WorkerSlot {
    pub(super) fn is_started(&self) -> bool {
        self.handle.get().is_some()
    }

    /// Spawn the worker once. The thread holds a `Weak<FlushHub>` plus
    /// clones of the wait cell, cancel token, and policy — never a
    /// strong `Arc<FlushHub>` — so the hub can reach refcount zero and
    /// run [`FlushHub`]'s `Drop`, which cancels and joins this thread.
    pub(super) fn start_with(&self, hub: &Arc<FlushHub>) {
        self.handle.get_or_init(|| {
            let weak = Arc::downgrade(hub);
            let wait = Arc::clone(&hub.wait);
            let cancel = hub.cancel.clone();
            let policy = hub.policy.clone();
            spawn_named("kithara-flush-hub", move || {
                run(&weak, &wait, &cancel, &policy);
            })
        });
    }

    /// Cancel-driven shutdown join. Joins the worker thread so it is
    /// gone by the time the hub drops — except when `Drop` is itself
    /// running on the worker thread (the worker upgraded the last
    /// `Arc<FlushHub>` during a flush and is dropping it), where a join
    /// would self-deadlock; there the thread is already unwinding to
    /// exit, so the handle is simply detached.
    pub(super) fn shutdown_join(&mut self) {
        if let Some(handle) = self.handle.take()
            && handle.thread().id() != std::thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

/// Background flush loop. Coalesces a burst of `signal()` calls through
/// `policy.debounce` (bypassed once `op_count` reaches
/// `force_every_n_ops`) before draining dirty sources. Exits on cancel
/// after a final flush, or when the hub has been dropped.
fn run(weak: &Weak<FlushHub>, wait: &HubWait, cancel: &CancellationToken, policy: &FlushPolicy) {
    loop {
        let mut guard = wait.state.lock_sync();
        while !guard.pending {
            if cancel.is_cancelled() {
                drop(guard);
                final_flush(weak);
                return;
            }
            let deadline = Instant::now() + policy.poll_interval;
            guard = wait.cv.wait_sync_timeout(guard, deadline);
        }
        drop(guard);

        let debounce_deadline = Instant::now() + policy.debounce;
        loop {
            let guard = wait.state.lock_sync();
            if guard.op_count >= policy.force_every_n_ops.get() {
                break;
            }
            if cancel.is_cancelled() {
                drop(guard);
                final_flush(weak);
                return;
            }
            if Instant::now() >= debounce_deadline {
                break;
            }
            let _ = wait.cv.wait_sync_timeout(guard, debounce_deadline);
        }

        {
            let mut guard = wait.state.lock_sync();
            guard.pending = false;
            guard.op_count = 0;
        }
        let Some(hub) = weak.upgrade() else {
            return;
        };
        let _g = hub.flush_lock.lock_sync();
        let _ = hub.flush_dirty(false);
    }
}

/// Final best-effort flush on shutdown. No-op if the hub is already
/// gone (its sources were dropped alongside it).
fn final_flush(weak: &Weak<FlushHub>) {
    let Some(hub) = weak.upgrade() else {
        return;
    };
    let _g = hub.flush_lock.lock_sync();
    let _ = hub.flush_dirty(false);
}
