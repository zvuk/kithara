//! Sim-participating `tokio::sync::oneshot` under `sim-time` (native).
//!
//! Same rationale as the sibling [`mpsc`](super::mpsc): the single value handoff
//! goes through the quiescence engine (an untimed channel waiter) instead of the
//! runtime reactor, so a receiver awaiting the response keeps a participant
//! `active` and the virtual clock cannot race past the send. Off the sim path
//! this module is not compiled and callers get the real `tokio` oneshot.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use parking_lot::Mutex;

use crate::time::sim::sched;

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
}

struct Shared<T> {
    inner: Mutex<Inner<T>>,
    cvid: u64,
}

/// Create a one-shot channel.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            value: None,
            sender_alive: true,
            receiver_alive: true,
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
        drop(inner);
        sched::signal_channel(shared.cvid, false);
        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            shared.inner.lock().sender_alive = false;
            // Wake the receiver so its next poll observes the closed sender.
            sched::signal_channel(shared.cvid, false);
        }
    }
}

/// Receiving half (a future resolving to the sent value or [`RecvError`]).
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    pending: Option<sched::AsyncHandle>,
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(handle) = this.pending.as_ref() {
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
        let (handle, adv) = sched::register_channel_async(this.shared.cvid, cx.waker().clone());
        this.pending = Some(handle);
        drop(inner);
        sched::fire_advance(adv);
        Poll::Pending
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.inner.lock().receiver_alive = false;
        if let Some(handle) = self.pending.take() {
            sched::cancel_async_wait(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;
    use kithara_test_utils::kithara;

    use super::{RecvError, channel};
    use crate::{time::sim, tokio::task::spawn};

    const ROUNDS: usize = 256;

    /// The exact download-stack handoff: a spawned task does (here, trivial)
    /// work then `send`s the result on a oneshot; an awaiter parks on the
    /// receiver. A lost wakeup would leave the awaiter parked forever (engine
    /// deadlock, real-time hang). Many concurrent rounds across worker threads
    /// stress the register-under-lock vs. send race.
    #[kithara::test(tokio, multi_thread)]
    async fn round_trip_no_lost_wakeup() {
        sim::reset();
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
        sim::reset();
        let (tx, rx) = channel::<usize>();
        drop(spawn(async move {
            drop(tx);
        }));
        assert_eq!(rx.await, Err(RecvError));
    }
}
