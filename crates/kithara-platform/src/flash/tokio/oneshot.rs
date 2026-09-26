use std::{
    future::Future,
    panic::Location,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use parking_lot::Mutex;

use crate::{
    flash::{
        diag::PrimKind,
        flash_ambient,
        ids::{Backend, trace_native_from_ambient},
        system,
    },
    sync::Arc,
};

/// Error observed when the sender drops without sending.
///
/// A distinct type from `tokio`'s (whose field is private, so it cannot be
/// constructed here); callers only ever map it away, never name it.
pub mod error {
    /// The sender half dropped without sending a value.
    #[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
    #[display("oneshot channel closed without a value")]
    #[derive(derive_more::Error)]
    #[error(ignore)]
    pub struct RecvError;
}

pub use error::RecvError;

struct Inner<T> {
    /// Real-wake slot for the single receiver (off the flash path). Stored under
    /// this mutex, after the receiver re-checks `value`/`sender_alive`, so a
    /// concurrent `send`/sender-drop (which takes the same mutex, then wakes)
    /// cannot slip its wake between the receiver's check and its park.
    real_waker: Option<Waker>,
    value: Option<T>,
    receiver_alive: bool,
    sender_alive: bool,
}

struct Shared<T> {
    /// Park/wake mechanism latched ONCE at `channel()` (see
    /// [`crate::flash::ids::Backend`]): both halves use it regardless of the
    /// calling thread's ambient, so a sender on a thread that did not inherit
    /// the test's ambient still reaches an engine-parked receiver.
    backend: Backend,
    inner: Mutex<Inner<T>>,
}

/// How the [`Receiver`] future parked, so re-poll and `Drop` use the matching
/// teardown — engine cancel vs. clearing the stored real waker.
enum Parked {
    Engine(system::AsyncHandle),
    Real,
}

/// Create a one-shot channel.
#[must_use]
#[track_caller]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            value: None,
            sender_alive: true,
            receiver_alive: true,
            real_waker: None,
        }),
        backend: if flash_ambient() {
            let cvid = system::next_condvar_id();
            system::describe_cvid(cvid, PrimKind::Oneshot, Location::caller());
            Backend::Engine(cvid)
        } else {
            Backend::Native
        },
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
    ///
    /// Takes the receiver's waker under the same lock it parks under, so storing the value and
    /// waking it stay atomic with respect to a receiver poll.
    pub fn send(mut self, value: T) -> Result<(), T> {
        let Some(shared) = self.shared.take() else {
            return Err(value);
        };
        let mut inner = shared.inner.lock();
        if !inner.receiver_alive {
            return Err(value);
        }
        inner.value = Some(value);
        let waker = inner.real_waker.take();
        drop(inner);
        match shared.backend {
            Backend::Engine(cvid) => system::signal_channel(cvid, false),
            Backend::Native => {
                trace_native_from_ambient("oneshot", "send");
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
        }
        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    /// Wakes the receiver so its next poll observes the sender as dropped.
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            let mut inner = shared.inner.lock();
            inner.sender_alive = false;
            let waker = inner.real_waker.take();
            drop(inner);
            match shared.backend {
                Backend::Engine(cvid) => system::signal_channel(cvid, false),
                Backend::Native => {
                    trace_native_from_ambient("oneshot", "sender_drop");
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                }
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

    /// Stores its waker under the lock, after re-checking value and alive state, so a concurrent
    /// send or sender-drop either observes the waker or has not yet stored what this poll missed.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
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
        match this.shared.backend {
            Backend::Engine(cvid) => {
                let (handle, adv) = system::register_channel_async(cvid, cx.waker().clone());
                this.pending = Some(Parked::Engine(handle));
                drop(inner);
                adv.fire();
            }
            Backend::Native => {
                trace_native_from_ambient("oneshot", "recv_park");
                inner.real_waker = Some(cx.waker().clone());
                this.pending = Some(Parked::Real);
                drop(inner);
            }
        }
        Poll::Pending
    }
}

impl<T> Drop for Receiver<T> {
    /// Drops its stored waker on a real park exit, so a late `send` does not wake an
    /// already-dropped future; the slot holds at most this receiver's waker.
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock();
        inner.receiver_alive = false;
        if matches!(self.pending, Some(Parked::Real)) {
            inner.real_waker = None;
        }
        drop(inner);
        if let Some(Parked::Engine(handle)) = self.pending.take() {
            system::cancel_async_wait(&handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::future::join_all;
    use kithara_test_utils::kithara;

    use super::{RecvError, channel};
    use crate::{consts, flash, tokio::task::spawn};

    /// The exact download-stack handoff: a spawned task does (here, trivial)
    /// work then `send`s the result on a oneshot; an awaiter parks on the
    /// receiver. A lost wakeup would leave the awaiter parked forever (engine
    /// deadlock, real-time hang). Many concurrent rounds across worker threads
    /// stress the register-under-lock vs. send race.
    #[kithara::test(tokio, multi_thread)]
    async fn round_trip_no_lost_wakeup() {
        flash::reset();
        let futs = (0..consts::ROUNDS).map(|r| async move {
            let (tx, rx) = channel::<usize>();
            drop(spawn(async move {
                tx.send(r * 2).ok();
            }));
            rx.await.expect("sender delivered")
        });
        let got: Vec<usize> = join_all(futs).await;
        let sum: usize = got.iter().sum();
        assert_eq!(sum, (0..consts::ROUNDS).map(|r| r * 2).sum::<usize>());
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
