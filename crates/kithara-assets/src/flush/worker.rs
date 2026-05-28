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
}

/// Outcome of a wait phase: keep coalescing, or shut the worker down
/// (cancel observed — the caller runs a final flush and exits).
enum Phase {
    Proceed,
    Shutdown,
}

/// Background flush loop. Coalesces a burst of `signal()` calls through
/// `policy.debounce` (bypassed once `op_count` reaches
/// `force_every_n_ops`) before draining dirty sources. Exits on cancel
/// after a final flush, or when the hub has been dropped.
fn run(weak: &Weak<FlushHub>, wait: &HubWait, cancel: &CancellationToken, policy: &FlushPolicy) {
    loop {
        if let Phase::Shutdown = await_flush_window(wait, cancel, policy) {
            return final_flush(weak);
        }
        clear_pending(wait);
        match weak.upgrade() {
            Some(hub) => flush_once(&hub),
            None => return,
        }
    }
}

/// Drain the flush window: park until a source is dirty, then coalesce
/// a burst of signals through the debounce. Returns [`Phase::Shutdown`]
/// when cancel fires in either phase (caller runs the final flush).
fn await_flush_window(wait: &HubWait, cancel: &CancellationToken, policy: &FlushPolicy) -> Phase {
    if let Phase::Shutdown = park_until_pending(wait, cancel, policy) {
        return Phase::Shutdown;
    }
    debounce(wait, cancel, policy)
}

/// Park on the wait cell until a source is marked dirty. Returns
/// [`Phase::Shutdown`] when cancel fires while idle.
fn park_until_pending(wait: &HubWait, cancel: &CancellationToken, policy: &FlushPolicy) -> Phase {
    let mut guard = wait.state.lock_sync();
    while !guard.pending {
        if cancel.is_cancelled() {
            return Phase::Shutdown;
        }
        let deadline = Instant::now() + policy.poll_interval;
        guard = wait.cv.wait_sync_timeout(guard, deadline);
    }
    drop(guard);
    Phase::Proceed
}

/// Coalesce a burst of signals for up to `policy.debounce`, bypassed
/// once `op_count` reaches `force_every_n_ops`. Returns
/// [`Phase::Shutdown`] when cancel fires mid-debounce.
fn debounce(wait: &HubWait, cancel: &CancellationToken, policy: &FlushPolicy) -> Phase {
    let deadline = Instant::now() + policy.debounce;
    loop {
        let guard = wait.state.lock_sync();
        match (
            guard.op_count >= policy.force_every_n_ops.get(),
            cancel.is_cancelled(),
            Instant::now() >= deadline,
        ) {
            (true, _, _) => return Phase::Proceed,
            (_, true, _) => return Phase::Shutdown,
            (_, _, true) => return Phase::Proceed,
            _ => {
                let _ = wait.cv.wait_sync_timeout(guard, deadline);
            }
        }
    }
}

/// Reset the coalescing state at the start of a flush cycle.
fn clear_pending(wait: &HubWait) {
    let mut guard = wait.state.lock_sync();
    guard.pending = false;
    guard.op_count = 0;
}

/// Take the flush lock and drain dirty sources once (best-effort).
fn flush_once(hub: &Arc<FlushHub>) {
    let _g = hub.flush_lock.lock_sync();
    let _ = hub.flush_dirty(false);
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
