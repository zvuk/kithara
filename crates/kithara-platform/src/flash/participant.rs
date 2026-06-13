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
    sync::Arc,
    task::{Context, Poll, Waker},
};

use pin_project_lite::pin_project;

use super::system::{self, credit, gate::TaskGate};

pin_project! {
    /// Spawn poll-wrapper: keeps the wrapped task counted in the engine's
    /// `active_async` for as long as it is non-quiescent — from becoming runnable
    /// (spawned, or woken) until it next parks, completes, or drops — via its
    /// [`TaskGate`]. The gate (not the per-poll bracket) is what closes the wake→poll
    /// window: a task whose waker has fired but which has not yet been re-polled
    /// stays counted, so the virtual clock cannot advance past a runnable task.
    /// Installed at the spawn chokepoint ([`crate::tokio::task::spawn`]) and on the
    /// test root task, so every async task on the sim path participates.
    pub struct Participating<F> {
        #[pin]
        fut: F,
        gate: Arc<TaskGate>,
    }

    impl<F> PinnedDrop for Participating<F> {
        fn drop(this: Pin<&mut Self>) {
            this.gate.on_drop();
        }
    }
}

impl<F: Future> Future for Participating<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        this.gate.store_runtime_waker(cx.waker());
        if !this.gate.try_enter_poll() {
            // Duplicate/stale schedule: the task is parked (or done), holding no
            // slot. Stay pending without re-polling the inner future — the real
            // wake will re-arm it.
            return Poll::Pending;
        }
        let gate_waker = Waker::from(Arc::clone(this.gate));
        let mut gate_cx = Context::from_waker(&gate_waker);
        // Mark this OS thread as inside an async poll for the duration of the
        // inner poll, so a synchronous wrapped wait taken from within it (e.g. a
        // blocking `recv` reaching the engine) is treated as a BRIDGED wait —
        // releasing this task's `active_async` slot while it blocks instead of
        // pinning the clock. Drops (restoring the depth) even if the poll unwinds.
        let outcome = {
            let _poll_guard = credit::AsyncPollGuard::enter();
            this.fut.poll(&mut gate_cx)
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

/// Wrap `fut` so it participates in quiescence accounting (see [`Participating`]).
/// The task occupies an `active_async` slot immediately — a constructed/spawned
/// task is runnable until polled — balanced when it parks, completes, or drops.
pub fn participate<F: Future>(fut: F) -> Participating<F> {
    system::async_acquire();
    Participating {
        fut,
        gate: TaskGate::new(),
    }
}
