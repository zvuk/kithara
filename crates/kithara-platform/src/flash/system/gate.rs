//! Per-task gate FSM for quiescence accounting — the engine half of
//! [`crate::flash::Participating`] (the control-surface wrapper stays in
//! `flash/participant.rs`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    task::{Wake, Waker},
};

use super::FLASH;
use crate::native::sync::Mutex;

/// Per-task quiescence states. The task occupies one `active_async` slot
/// while it is in any non-quiescent state ([`Runnable`](TaskState::Runnable),
/// [`Running`](TaskState::Running), [`RunningNotified`](TaskState::RunningNotified));
/// it releases the slot only on the transition to [`Parked`](TaskState::Parked),
/// on completion, or on drop.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TaskState {
    Parked = 0,
    Runnable = 1,
    Running = 2,
    RunningNotified = 3,
    Done = 4,
}

/// Typed atomic over [`TaskState`]: the gate FSM cell. The orderings are fixed
/// per operation (CAS `AcqRel`/`Acquire`, swap `AcqRel`, store `Release`, load
/// `Acquire`) — exactly the orderings the untyped `AtomicU8` sites used.
pub(super) struct AtomicTaskState(AtomicU8);

impl AtomicTaskState {
    fn new(initial: TaskState) -> Self {
        Self(AtomicU8::new(initial as u8))
    }

    fn unpack(v: u8) -> TaskState {
        match v {
            0 => TaskState::Parked,
            1 => TaskState::Runnable,
            2 => TaskState::Running,
            3 => TaskState::RunningNotified,
            4 => TaskState::Done,
            // Only `TaskState` discriminants are ever stored in the cell.
            _ => unreachable!("BUG: invalid TaskState discriminant {v}"),
        }
    }

    pub(super) fn load(&self) -> TaskState {
        Self::unpack(self.0.load(Ordering::Acquire))
    }

    pub(super) fn store(&self, new: TaskState) {
        self.0.store(new as u8, Ordering::Release);
    }

    pub(super) fn swap(&self, new: TaskState) -> TaskState {
        Self::unpack(self.0.swap(new as u8, Ordering::AcqRel))
    }

    /// CAS `current -> new`; `true` iff it transitioned.
    pub(super) fn compare_exchange(&self, current: TaskState, new: TaskState) -> bool {
        self.0
            .compare_exchange(
                current as u8,
                new as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// Outcome of a gate park attempt ([`super::FlashInner::gate_park`]).
pub(super) enum ParkOutcome {
    /// `Running -> Parked`: the slot was released (a quiescent edge).
    Parked,
    /// A wake landed mid-poll (`RunningNotified`), so the CAS failed: the gate
    /// stays runnable, keeping the slot for the re-poll that wake scheduled.
    WokenMidPoll,
}

/// Outcome of waking a parked gate ([`super::FlashInner::gate_wake_parked`]).
pub(super) enum WakeOutcome {
    /// `Parked -> Runnable`: the slot was re-acquired; forward the runtime waker.
    Resumed,
    /// The state left `Parked` between the load and the CAS; the caller's wake
    /// loop re-reads and handles the current state lock-free.
    NotParked,
}

/// Per-task gate for quiescence accounting. It tracks whether the task currently
/// occupies an `active_async` slot and INTERCEPTS every wake (it is handed to the
/// inner future as its `Waker`), so a task that has been woken — its waker fired
/// and it is queued to be polled — is counted from that instant until it is next
/// polled. This closes the wake→poll window the old per-poll wrapper left open
/// (a runnable-but-not-yet-repolled task was uncounted, so the clock could jump
/// past it). Because the gate IS the inner future's waker, EVERY wake routes
/// through it: engine wakes, the real-I/O reactor, `JoinHandle`, raw channels.
pub(in crate::flash) struct TaskGate {
    state: AtomicTaskState,
    /// The runtime's waker for this task, refreshed each poll. The gate forwards
    /// to it on wake so the real poll is re-scheduled.
    runtime_waker: Mutex<Option<Waker>>,
}

impl TaskGate {
    /// A fresh gate starts `Runnable`: a constructed/spawned task is queued to be
    /// polled, so it occupies a slot at once (acquired by
    /// [`crate::flash::participate`]).
    pub(in crate::flash) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicTaskState::new(TaskState::Runnable),
            runtime_waker: Mutex::new(None),
        })
    }

    pub(in crate::flash) fn store_runtime_waker(&self, w: &Waker) {
        let mut g = self.runtime_waker.lock();
        match g.as_ref() {
            Some(existing) if existing.will_wake(w) => {}
            _ => *g = Some(w.clone()),
        }
    }

    fn forward(&self) {
        let w = self.runtime_waker.lock().clone();
        if let Some(w) = w {
            w.wake();
        }
    }

    /// Poll entry: claim the poll iff the task is `RUNNABLE` (it holds a slot and
    /// was genuinely queued). Returns `false` for any other state — a
    /// duplicate/stale schedule (the runtime can poll more times than there are
    /// wakes when several `forward`s race a `park`). The caller MUST then return
    /// `Pending` without polling the inner future or touching the slot, so the
    /// `active_async` accounting stays balanced. The slot is already held from
    /// spawn or the waking transition, so a successful claim changes no counter.
    pub(in crate::flash) fn try_enter_poll(&self) -> bool {
        self.state
            .compare_exchange(TaskState::Runnable, TaskState::Running)
    }

    /// Poll returned `Ready`: the task is done — release its slot. The `DONE`
    /// store and the counter decrement happen together under the engine lock.
    pub(in crate::flash) fn complete(&self) {
        FLASH.gate_complete(&self.state);
    }

    /// Poll returned `Pending`: `RUNNING`→`PARKED` releases the slot (a quiescent
    /// edge); a wake that landed mid-poll left `RUNNING_NOTIFIED`, so the CAS fails
    /// and the gate stays `RUNNABLE`, keeping the slot for the re-poll that wake
    /// already scheduled. The state transition and the counter move atomically
    /// under the engine lock so a concurrent wake cannot interleave — see
    /// [`super::FlashInner::gate_park`]. Both [`ParkOutcome`] arms are fully
    /// handled under that lock, so the returned outcome needs no action here.
    pub(in crate::flash) fn park(&self) {
        let _: ParkOutcome = FLASH.gate_park(&self.state);
    }

    /// Drop: release the slot iff the task still occupies one (`RUNNABLE`/`RUNNING`/
    /// `RUNNING_NOTIFIED`). `PARKED` and `DONE` hold none.
    pub(in crate::flash) fn on_drop(&self) {
        FLASH.gate_drop_release(&self.state);
    }
}

impl Wake for TaskGate {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        loop {
            match self.state.load() {
                TaskState::Parked => {
                    // Re-acquire the slot the park released BEFORE the real poll
                    // runs: this wake→poll window must stay counted so the clock
                    // cannot jump past this task. The CAS and the acquire happen
                    // together under the engine lock so a concurrent `park`'s
                    // release cannot cancel this acquire — see
                    // [`super::FlashInner::gate_wake_parked`]. `NotParked` means the
                    // state left `Parked` between the load and the CAS; loop to re-read.
                    match FLASH.gate_wake_parked(&self.state) {
                        WakeOutcome::Resumed => {
                            self.forward();
                            return;
                        }
                        WakeOutcome::NotParked => {}
                    }
                }
                TaskState::Running => {
                    if self
                        .state
                        .compare_exchange(TaskState::Running, TaskState::RunningNotified)
                    {
                        // Woken during its own poll; slot already held. Forward so
                        // the runtime re-polls after the current poll returns.
                        self.forward();
                        return;
                    }
                }
                // Runnable / RunningNotified: already pending a poll, slot held —
                // idempotent. Done: nothing to wake.
                TaskState::Runnable | TaskState::RunningNotified => {
                    self.forward();
                    return;
                }
                TaskState::Done => return,
            }
        }
    }
}
