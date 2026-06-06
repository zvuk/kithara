//! Sim-participating `tokio::sync::oneshot` under `flash-time` (native).
//!
//! Same rationale as the sibling [`mpsc`](super::mpsc): the single value handoff
//! goes through the quiescence engine (an untimed channel waiter) instead of the
//! runtime reactor, so a receiver awaiting the response keeps a participant
//! `active` and the virtual clock cannot race past the send. Off the sim path
//! this module is not compiled and callers get the real `tokio` oneshot.
//!
//! Each wait/wake branches on [`flash_enabled`]: when flash governs the task the
//! handoff uses the engine waiter; otherwise the receiver stores its real
//! [`Waker`] in the shared state (under the SAME `inner` mutex that guards the
//! value) and the sender wakes it directly — a true reactor-free wake that does
//! not touch the engine's participant accounting. The queue/value/alive state is
//! UNIFIED; only the park/wake mechanism branches. This default-real path is the
//! only one taken until `#[kithara::flash]` annotations land.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use parking_lot::Mutex;

use crate::time::flash::{flash_enabled, sched};

/// Error observed when the sender drops without sending.
///
/// A distinct type from `tokio`'s (whose field is private, so it cannot be
/// constructed here); callers only ever map it away, never name it.
pub mod error {
    /// The sender half dropped without sending a value.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RecvError;

    impl std::fmt::Display for RecvError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("oneshot channel closed without a value")
        }
    }

    impl std::error::Error for RecvError {}
}

pub use error::RecvError;

struct Inner<T> {
    value: Option<T>,
    sender_alive: bool,
    receiver_alive: bool,
    /// Real-wake slot for the single receiver (off the flash path). Stored under
    /// this mutex, after the receiver re-checks `value`/`sender_alive`, so a
    /// concurrent `send`/sender-drop (which takes the same mutex, then wakes)
    /// cannot slip its wake between the receiver's check and its park.
    real_waker: Option<Waker>,
}

struct Shared<T> {
    inner: Mutex<Inner<T>>,
    cvid: u64,
}

/// How the [`Receiver`] future parked, so re-poll and `Drop` use the matching
/// teardown — engine cancel vs. clearing the stored real waker.
enum Parked {
    Engine(sched::AsyncHandle),
    Real,
}

/// Create a one-shot channel.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            value: None,
            sender_alive: true,
            receiver_alive: true,
            real_waker: None,
        }),
        cvid: sched::next_condvar_id(),
    });
    (
        Sender {
            shared: Some(Arc::clone(&shared)),
        },
        Receiver {
            shared,
            pending: None,
        },
    )
}

/// Sending half (consumed by [`send`](Sender::send)).
pub struct Sender<T> {
    shared: Option<Arc<Shared<T>>>,
}

impl<T> Sender<T> {
    /// Deliver `value`.
    ///
    /// # Errors
    /// Returns `Err(value)` when the receiver has already dropped.
    pub fn send(mut self, value: T) -> Result<(), T> {
        let Some(shared) = self.shared.take() else {
            return Err(value);
        };
        let mut inner = shared.inner.lock();
        if !inner.receiver_alive {
            return Err(value);
        }
        inner.value = Some(value);
        // Take the real waker under the same lock the receiver parks under, so
        // the value store and the wake are atomic w.r.t. a receiver poll.
        let waker = inner.real_waker.take();
        drop(inner);
        if flash_enabled() {
            sched::signal_channel(shared.cvid, false);
        } else if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            let mut inner = shared.inner.lock();
            inner.sender_alive = false;
            let waker = inner.real_waker.take();
            drop(inner);
            // Wake the receiver so its next poll observes the closed sender.
            if flash_enabled() {
                sched::signal_channel(shared.cvid, false);
            } else if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

/// Receiving half (a future resolving to the sent value or [`RecvError`]).
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    pending: Option<Parked>,
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Re-poll bookkeeping: an engine wait resolves only when granted; a real
        // wait always re-checks the value/alive state below (a spurious wake just
        // re-parks). Either way the parked marker is cleared so we re-evaluate.
        if let Some(Parked::Engine(handle)) = this.pending.as_ref() {
            if handle.granted() {
                this.pending = None;
            } else {
                return Poll::Pending;
            }
        }
        let mut inner = this.shared.inner.lock();
        if let Some(value) = inner.value.take() {
            return Poll::Ready(Ok(value));
        }
        if !inner.sender_alive {
            return Poll::Ready(Err(RecvError));
        }
        if flash_enabled() {
            let (handle, adv) = sched::register_channel_async(this.shared.cvid, cx.waker().clone());
            this.pending = Some(Parked::Engine(handle));
            drop(inner);
            sched::fire_advance(adv);
        } else {
            // Store the real waker UNDER the lock, after re-checking value/alive,
            // so a `send`/sender-drop that takes this lock either observes our
            // waker (and wakes it) or has not yet stored the value we just missed.
            inner.real_waker = Some(cx.waker().clone());
            this.pending = Some(Parked::Real);
        }
        Poll::Pending
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock();
        inner.receiver_alive = false;
        // Real park: drop our stored waker so a late sender does not wake a
        // dropped future. (The slot holds at most this receiver's waker.)
        if matches!(self.pending, Some(Parked::Real)) {
            inner.real_waker = None;
        }
        drop(inner);
        if let Some(Parked::Engine(handle)) = self.pending.take() {
            sched::cancel_async_wait(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;
    use kithara_test_utils::kithara;

    use super::{RecvError, channel};
    use crate::{time::flash, tokio::task::spawn};

    const ROUNDS: usize = 256;

    /// The exact download-stack handoff: a spawned task does (here, trivial)
    /// work then `send`s the result on a oneshot; an awaiter parks on the
    /// receiver. A lost wakeup would leave the awaiter parked forever (engine
    /// deadlock, real-time hang). Many concurrent rounds across worker threads
    /// stress the register-under-lock vs. send race.
    #[kithara::test(tokio, multi_thread)]
    async fn round_trip_no_lost_wakeup() {
        flash::reset();
        let futs = (0..ROUNDS).map(|r| async move {
            let (tx, rx) = channel::<usize>();
            drop(spawn(async move {
                let _ = tx.send(r * 2);
            }));
            rx.await.expect("sender delivered")
        });
        let got: Vec<usize> = join_all(futs).await;
        let sum: usize = got.iter().sum();
        assert_eq!(sum, (0..ROUNDS).map(|r| r * 2).sum::<usize>());
    }

    /// A sender dropped without sending must resolve the receiver with
    /// [`RecvError`], never park it forever.
    #[kithara::test(tokio, multi_thread)]
    async fn dropped_sender_resolves_recv_error() {
        flash::reset();
        let (tx, rx) = channel::<usize>();
        drop(spawn(async move {
            drop(tx);
        }));
        assert_eq!(rx.await, Err(RecvError));
    }
}
