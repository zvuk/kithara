use std::{
    future::Future,
    panic::Location,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use pin_project_lite::pin_project;

use super::{
    join::{Join, hand_over},
    system::{
        self,
        credit::{AsyncPollGuard, DedicatedSlot},
        gate::TaskGate,
    },
};
use crate::sync::Arc;

pin_project! {
    /// Spawn poll-wrapper: keeps the wrapped task counted in the engine's
    /// `active_async` for as long as it is non-quiescent — from becoming runnable
    /// (spawned, or woken) until it next parks, completes, or drops — via its
    /// [`TaskGate`]. The gate (not the per-poll bracket) is what closes the wake→poll
    /// window: a task whose waker has fired but which has not yet been re-polled
    /// stays counted, so the virtual clock cannot advance past a runnable task.
    /// Installed at the spawn chokepoint ([`crate::tokio::task::spawn`]) and on the
    /// test root task, so every async task on the sim path participates. A
    /// spawned task's slot outlives its end until its joiner is woken (see
    /// [`Join`]).
    pub struct Participating<F> {
        #[pin]
        fut: F,
        gate: Arc<TaskGate>,
        join: Option<Arc<Join>>,
    }

    impl<F> PinnedDrop for Participating<F> {
        fn drop(this: Pin<&mut Self>) {
            hand_over(this.join.as_deref(), this.gate.on_drop());
        }
    }
}

impl<F: Future> Future for Participating<F> {
    type Output = F::Output;

    /// A duplicate or stale wake is ignored: an already parked or done task stays pending without
    /// re-polling. The OS thread is marked inside an async poll for the inner poll's duration, so a
    /// blocking wait taken from within it counts as bridged.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        this.gate.store_runtime_waker(cx.waker());
        if !this.gate.try_enter_poll() {
            return Poll::Pending;
        }
        let gate_waker = Waker::from(Arc::clone(this.gate));
        let mut gate_cx = Context::from_waker(&gate_waker);
        let outcome = {
            let _poll_guard = AsyncPollGuard::enter(this.gate.id(), this.gate.loc());
            this.fut.poll(&mut gate_cx)
        };
        match outcome {
            Poll::Ready(out) => {
                hand_over(this.join.as_deref(), Some(this.gate.complete()));
                Poll::Ready(out)
            }
            Poll::Pending => {
                this.gate.park();
                Poll::Pending
            }
        }
    }
}

/// Wrap `fut` so it participates in quiescence accounting (see [`Participating`]).
/// The task occupies an `active_async` slot immediately — a constructed/spawned
/// task is runnable until polled — balanced when it parks, completes, or drops.
///
/// `loc` is the spawn site (a [`crate::tokio::task::spawn`] caller, forwarded via
/// `#[track_caller]`, or a direct test caller). It is the task's stable identity
/// in the engine's `active_async` holder map, so a quiescence-hang dump names
/// WHICH spawn pins the clock instead of printing a bare counter.
pub fn participate<F: Future>(fut: F, loc: &'static Location<'static>) -> Participating<F> {
    Participating {
        fut,
        gate: system::async_acquire(loc),
        join: None,
    }
}

/// Count synchronous driver work until it returns or enters a wrapped wait.
/// An enclosing thread or async participant already owns the required credit.
/// The closure must not drive an async runtime awaiting virtual timers.
#[track_caller]
pub fn participate_sync<F: FnOnce() -> R, R>(work: F) -> R {
    if !super::ctx::flash_ambient() || super::ctx::dedicated() || super::ctx::poll_depth() != 0 {
        return work();
    }
    let _participant = DedicatedSlot::reserve(Location::caller()).claim_pooled();
    work()
}

impl<F> Participating<F> {
    /// The task's `JoinHandle` holds `join`: the task's end hands its slot there.
    pub(crate) fn joined(mut self, join: Arc<Join>) -> Self {
        self.join = Some(join);
        self
    }
}
