//! Platform `Notify`: real `tokio::sync::Notify` off the sim path, an
//! engine-backed event wait under `flash-time`.
//!
//! The sim variant mirrors the sim [`Condvar`](super::Condvar): a fresh `cvid`
//! identifies the notify; `notified()` registers an untimed waiter (no deadline
//! — woken only by a `notify_*`, never by a clock jump, matching tokio Notify);
//! `notify_one`/`notify_waiters` signal that group. A `notify_one` with no
//! waiter stores a permit so the next `notified()` resolves immediately. The
//! wait collapses to zero wall-clock and participates in quiescence.

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
use crate::time::sim::sched;
/// Real-tokio `Notify` (off the sim path): a transparent re-export so callers
/// use one type everywhere.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash-time")))]
pub use crate::tokio::sync::Notify;

/// Engine-backed `Notify` under `flash-time`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub struct Notify {
    cvid: u64,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
impl Default for Notify {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
impl Notify {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cvid: sched::next_condvar_id(),
        }
    }

    pub fn notify_one(&self) {
        sched::signal_notify(self.cvid, false);
    }

    pub fn notify_waiters(&self) {
        sched::signal_notify(self.cvid, true);
    }

    pub fn notified(&self) -> Notified<'_> {
        Notified {
            cvid: self.cvid,
            handle: None,
            resolved_by_permit: false,
            _notify: self,
        }
    }
}

/// Future returned by [`Notify::notified`] under `flash-time`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub struct Notified<'a> {
    cvid: u64,
    handle: Option<sched::AsyncHandle>,
    resolved_by_permit: bool,
    _notify: &'a Notify,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
impl Future for Notified<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.resolved_by_permit {
            return Poll::Ready(());
        }
        if let Some(handle) = self.handle.as_ref() {
            if handle.granted() {
                // The firer (notify_*) selected us; resolve. The task's
                // `active_async` count is owned by the spawn poll-wrapper, so
                // resolve touches no counter.
                self.handle = None;
                return Poll::Ready(());
            }
            // Spurious re-poll before a signal: stay parked (engine never wakes
            // an untimed waiter on a clock jump).
            return Poll::Pending;
        }
        let (handle, adv) = sched::register_notify_async(self.cvid, cx.waker().clone());
        match handle {
            None => {
                // A notify_one had landed with no waiter: consume the permit and
                // resolve at once, without parking (no slot was granted).
                self.resolved_by_permit = true;
                Poll::Ready(())
            }
            Some(handle) => {
                self.handle = Some(handle);
                sched::fire_advance(adv);
                Poll::Pending
            }
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
impl Drop for Notified<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            sched::cancel_async_wait(handle);
        }
    }
}
