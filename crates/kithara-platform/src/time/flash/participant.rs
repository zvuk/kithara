//! Per-task quiescence accounting for the flash engine.
//!
//! The [`TaskGate`] tracks whether a task currently occupies an `active_async`
//! slot and INTERCEPTS every wake (it is handed to the inner future as its
//! `Waker`), so a task that has been woken — its waker fired and it is queued to
//! be polled — is counted from that instant until it is next polled. [`participate`]
//! wraps a future in a [`Participating`] gate at the spawn chokepoint so every
//! async task on the sim path participates.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use parking_lot::Mutex;

use super::sched;

/// Per-task gate for quiescence accounting. It tracks whether the task currently
/// occupies an `active_async` slot and INTERCEPTS every wake (it is handed to the
/// inner future as its `Waker`), so a task that has been woken — its waker fired
/// and it is queued to be polled — is counted from that instant until it is next
/// polled. This closes the wake→poll window the old per-poll wrapper left open
/// (a runnable-but-not-yet-repolled task was uncounted, so the clock could jump
/// past it). Because the gate IS the inner future's waker, EVERY wake routes
/// through it: engine wakes, the real-I/O reactor, `JoinHandle`, raw channels.
struct TaskGate {
    state: AtomicU8,
    /// The runtime's waker for this task, refreshed each poll. The gate forwards
    /// to it on wake so the real poll is re-scheduled.
    runtime_waker: Mutex<Option<Waker>>,
}

impl TaskGate {
    /// Per-task quiescence states. The task occupies one `active_async` slot
    /// while it is in any non-quiescent state (`RUNNABLE`, `RUNNING`,
    /// `RUNNING_NOTIFIED`); it releases the slot only on the transition to
    /// `PARKED`, on completion, or on drop.
    const PARKED: u8 = 0;
    const RUNNABLE: u8 = 1;
    const RUNNING: u8 = 2;
    const RUNNING_NOTIFIED: u8 = 3;
    const DONE: u8 = 4;

    /// A fresh gate starts `RUNNABLE`: a constructed/spawned task is queued to be
    /// polled, so it occupies a slot at once (acquired by [`participate`]).
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(Self::RUNNABLE),
            runtime_waker: Mutex::new(None),
        })
    }

    fn store_runtime_waker(&self, w: &Waker) {
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
    fn try_enter_poll(&self) -> bool {
        self.state
            .compare_exchange(
                Self::RUNNABLE,
                Self::RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Poll returned `Ready`: the task is done — release its slot. The `DONE`
    /// store and the counter decrement happen together under the `SCHED` lock.
    fn complete(&self) {
        sched::gate_complete(&self.state, Self::DONE);
    }

    /// Poll returned `Pending`: `RUNNING`→`PARKED` releases the slot (a quiescent
    /// edge); a wake that landed mid-poll left `RUNNING_NOTIFIED`, so the CAS fails
    /// and the gate stays `RUNNABLE`, keeping the slot for the re-poll that wake
    /// already scheduled. The state transition and the counter move atomically
    /// under the `SCHED` lock so a concurrent wake cannot interleave — see
    /// [`sched::gate_park`].
    fn park(&self) {
        sched::gate_park(&self.state, Self::RUNNING, Self::PARKED, Self::RUNNABLE);
    }

    /// Drop: release the slot iff the task still occupies one (`RUNNABLE`/`RUNNING`/
    /// `RUNNING_NOTIFIED`). `PARKED` and `DONE` hold none.
    fn on_drop(&self) {
        sched::gate_drop_release(
            &self.state,
            Self::DONE,
            Self::RUNNABLE,
            Self::RUNNING,
            Self::RUNNING_NOTIFIED,
        );
    }
}

impl Wake for TaskGate {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        loop {
            match self.state.load(Ordering::Acquire) {
                Self::PARKED => {
                    // Re-acquire the slot the park released BEFORE the real poll
                    // runs: this wake→poll window must stay counted so the clock
                    // cannot jump past this task. The CAS and the acquire happen
                    // together under the `SCHED` lock so a concurrent `park`'s
                    // release cannot cancel this acquire — see
                    // [`sched::gate_wake_parked`]. A `false` return means the state
                    // left `PARKED` between the load and the CAS; loop to re-read.
                    if sched::gate_wake_parked(&self.state, Self::PARKED, Self::RUNNABLE) {
                        self.forward();
                        return;
                    }
                }
                Self::RUNNING => {
                    if self
                        .state
                        .compare_exchange(
                            Self::RUNNING,
                            Self::RUNNING_NOTIFIED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        // Woken during its own poll; slot already held. Forward so
                        // the runtime re-polls after the current poll returns.
                        self.forward();
                        return;
                    }
                }
                // RUNNABLE / RUNNING_NOTIFIED: already pending a poll, slot held —
                // idempotent. DONE: nothing to wake.
                Self::RUNNABLE | Self::RUNNING_NOTIFIED => {
                    self.forward();
                    return;
                }
                _ => return,
            }
        }
    }
}

/// Spawn poll-wrapper: keeps the wrapped task counted in the engine's
/// `active_async` for as long as it is non-quiescent — from becoming runnable
/// (spawned, or woken) until it next parks, completes, or drops — via its
/// [`TaskGate`]. The gate (not the per-poll bracket) is what closes the wake→poll
/// window: a task whose waker has fired but which has not yet been re-polled
/// stays counted, so the virtual clock cannot advance past a runnable task.
/// Installed at the spawn chokepoint ([`crate::tokio::task::spawn`]) and on the
/// test root task, so every async task on the sim path participates.
pub struct Participating<F> {
    fut: F,
    gate: Arc<TaskGate>,
}

impl<F: Future> Future for Participating<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: `fut` is structurally pinned and never moved out; `gate`
        // (`Arc`) is `Unpin` and only touched through shared refs. We re-pin
        // `fut` in place via `Pin::new_unchecked`, so projecting the outer pin
        // with `get_unchecked_mut` is sound.
        let this = unsafe { self.get_unchecked_mut() };
        this.gate.store_runtime_waker(cx.waker());
        if !this.gate.try_enter_poll() {
            // Duplicate/stale schedule: the task is parked (or done), holding no
            // slot. Stay pending without re-polling the inner future — the real
            // wake will re-arm it.
            return Poll::Pending;
        }
        let gate_waker = Waker::from(Arc::clone(&this.gate));
        let mut gate_cx = Context::from_waker(&gate_waker);
        // SAFETY: `fut` is structurally pinned, never moved out of the participant
        // wrapper; it is re-pinned in place for its own poll.
        let fut = unsafe { Pin::new_unchecked(&mut this.fut) };
        // Mark this OS thread as inside an async poll for the duration of the
        // inner poll, so a synchronous wrapped wait taken from within it (e.g. a
        // blocking `recv_sync` reaching the engine) is treated as a BRIDGED wait —
        // releasing this task's `active_async` slot while it blocks instead of
        // pinning the clock. Drops (restoring the depth) even if the poll unwinds.
        let outcome = {
            let _poll_guard = sched::AsyncPollGuard::enter();
            fut.poll(&mut gate_cx)
        };
        match outcome {
            Poll::Ready(out) => {
                this.gate.complete();
                Poll::Ready(out)
            }
            Poll::Pending => {
                this.gate.park();
                Poll::Pending
            }
        }
    }
}

impl<F> Drop for Participating<F> {
    fn drop(&mut self) {
        self.gate.on_drop();
    }
}

/// Wrap `fut` so it participates in quiescence accounting (see [`Participating`]).
/// The task occupies an `active_async` slot immediately — a constructed/spawned
/// task is runnable until polled — balanced when it parks, completes, or drops.
pub fn participate<F: Future>(fut: F) -> Participating<F> {
    sched::async_acquire();
    Participating {
        fut,
        gate: TaskGate::new(),
    }
}
