use std::{
    future::Future,
    panic::Location,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use parking_lot::Mutex;

use crate::{
    backend::tokio::sync::broadcast as inner,
    flash::{
        diag::PrimKind,
        flash_ambient,
        ids::{Backend, trace_native_from_ambient},
        system,
    },
    sync::Arc,
};

/// Receive errors, mirroring `tokio::broadcast::error` variant-for-variant so
/// every consumer `match` arm keeps compiling. Distinct types from `tokio`'s
/// (whose internals are private), produced by mapping the inner ring's errors.
pub mod error {
    /// Failure of an awaited `recv`.
    #[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq, derive_more::Error)]
    #[error(ignore)]
    pub enum RecvError {
        /// All senders dropped and the ring is drained.
        #[display("channel closed")]
        Closed,
        /// This receiver fell behind; `n` messages were skipped. Recoverable.
        #[display("channel lagged by {_0}")]
        Lagged(u64),
    }

    /// Failure of a non-blocking `try_recv`.
    #[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq, derive_more::Error)]
    #[error(ignore)]
    pub enum TryRecvError {
        /// No message is currently buffered for this receiver.
        #[display("channel empty")]
        Empty,
        /// All senders dropped and the ring is drained.
        #[display("channel closed")]
        Closed,
        /// This receiver fell behind; `n` messages were skipped. Recoverable.
        #[display("channel lagged by {_0}")]
        Lagged(u64),
    }

    /// Returned by `Sender::send` when there are no live receivers; carries the
    /// value back. Callers typically discard it via `.ok()`.
    #[derive(derive_more::Debug, derive_more::Display)]
    #[debug("SendError(..)")]
    #[display("sending on a channel with no receivers")]
    #[derive(derive_more::Error)]
    #[error(ignore)]
    pub struct SendError<T>(pub T);
}

pub use error::{RecvError, SendError, TryRecvError};

/// Wake coordination shared by both halves (not generic over the payload): the
/// gate (off-flash parked-receiver wakers + the `closed` latch) plus the live
/// sender count and the construction-latched [`Backend`].
struct Shared {
    /// Live sender handles; the last to drop closes the channel.
    senders: AtomicUsize,
    backend: Backend,
    gate: Mutex<Gate>,
}

#[derive(Default)]
struct Gate {
    /// Off-flash real wakers for parked receivers; drained on each signal.
    wakers: Vec<Waker>,
    /// Set by the last sender's drop so a receiver re-checking before the inner
    /// tokio sender finishes dropping still resolves `Closed`.
    closed: bool,
}

impl Shared {
    /// Mark the channel closed and wake every parked receiver so each observes
    /// it. Called by the last sender's drop.
    fn close(&self) {
        let mut gate = self.gate.lock();
        gate.closed = true;
        let drained = std::mem::take(&mut gate.wakers);
        drop(gate);
        match self.backend {
            Backend::Engine(cvid) => system::signal_channel(cvid, true),
            Backend::Native => {
                trace_native_from_ambient("broadcast", "close");
                for waker in drained {
                    waker.wake();
                }
            }
        }
    }

    /// Wake every parked receiver so each re-checks the ring (a broadcast send is
    /// visible to all). Called AFTER the inner `send`.
    fn signal(&self) {
        let mut gate = self.gate.lock();
        let drained = std::mem::take(&mut gate.wakers);
        drop(gate);
        match self.backend {
            Backend::Engine(cvid) => system::signal_channel(cvid, true),
            Backend::Native => {
                trace_native_from_ambient("broadcast", "send");
                for waker in drained {
                    waker.wake();
                }
            }
        }
    }
}

/// Create a bounded broadcast channel buffering `capacity` messages per receiver.
#[must_use]
#[track_caller]
pub fn channel<T: Clone>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = inner::channel(capacity);
    let shared = Arc::new(Shared {
        gate: Mutex::new(Gate::default()),
        senders: AtomicUsize::new(1),
        backend: if flash_ambient() {
            let cvid = system::next_condvar_id();
            system::describe_cvid(cvid, PrimKind::Broadcast, Location::caller());
            Backend::Engine(cvid)
        } else {
            Backend::Native
        },
    });
    (
        Sender {
            inner: tx,
            shared: Arc::clone(&shared),
        },
        Receiver {
            shared,
            inner: rx,
            pending: None,
        },
    )
}

/// Broadcast sender (clone for multi-producer).
pub struct Sender<T> {
    shared: Arc<Shared>,
    inner: inner::Sender<T>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.senders.fetch_add(1, Ordering::AcqRel);
        Self {
            inner: self.inner.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for Sender<T> {
    /// Marks the channel closed and signals before the inner sender field drops, so a receiver
    /// woken during that window still observes `Closed`.
    fn drop(&mut self) {
        if self.shared.senders.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.close();
        }
    }
}

impl<T> Sender<T> {
    /// Number of live receivers.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.inner.receiver_count()
    }

    /// Subscribe a new receiver to subsequently-sent messages.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<T> {
        Receiver {
            inner: self.inner.subscribe(),
            shared: Arc::clone(&self.shared),
            pending: None,
        }
    }
}

impl<T: Clone> Sender<T> {
    /// Broadcast `value` to all current receivers, then wake any parked.
    ///
    /// # Errors
    /// Returns the value back when there are no live receivers.
    ///
    /// Signals after the inner append completes, preserving the lost-wakeup handshake with parked
    /// receivers.
    pub fn send(&self, value: T) -> Result<usize, SendError<T>> {
        let result = self.inner.send(value);
        self.shared.signal();
        result.map_err(|e| SendError(e.0))
    }
}

/// Broadcast receiver (one independent cursor into the ring).
pub struct Receiver<T> {
    shared: Arc<Shared>,
    pending: Option<Parked>,
    inner: inner::Receiver<T>,
}

/// How a [`Recv`] parked, so re-poll and `Drop` use the matching teardown.
/// `Real` carries the waker clone for exact-entry removal on `Drop`.
enum Parked {
    Engine(system::AsyncHandle),
    Real(Waker),
}

impl<T: Clone> Receiver<T> {
    /// Receive the next message, awaiting one while the channel is open and empty.
    pub fn recv(&mut self) -> Recv<'_, T> {
        Recv { rx: self }
    }

    /// Try to receive without awaiting.
    ///
    /// # Errors
    /// [`TryRecvError::Empty`] when open but empty, [`TryRecvError::Lagged`] when
    /// this receiver fell behind, [`TryRecvError::Closed`] once all senders
    /// dropped and the ring is drained.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self.inner.try_recv() {
            Ok(value) => Ok(value),
            Err(inner::error::TryRecvError::Empty) => Err(TryRecvError::Empty),
            Err(inner::error::TryRecvError::Closed) => Err(TryRecvError::Closed),
            Err(inner::error::TryRecvError::Lagged(n)) => Err(TryRecvError::Lagged(n)),
        }
    }
}

/// Future returned by [`Receiver::recv`].
pub struct Recv<'a, T> {
    rx: &'a mut Receiver<T>,
}

impl<T: Clone> Future for Recv<'_, T> {
    type Output = Result<T, RecvError>;

    /// Holds the gate across the try-receive and the waiter registration, so a concurrent `send` is
    /// either observed by this `try_recv` or wakes the waiter registered here.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let rx = &mut *self.get_mut().rx;
        match rx.pending.as_ref() {
            Some(Parked::Engine(handle)) => {
                if handle.granted() {
                    rx.pending = None;
                } else {
                    return Poll::Pending;
                }
            }
            Some(Parked::Real(_)) => rx.pending = None,
            None => {}
        }
        let mut gate = rx.shared.gate.lock();
        match rx.inner.try_recv() {
            Ok(value) => {
                drop(gate);
                Poll::Ready(Ok(value))
            }
            Err(inner::error::TryRecvError::Lagged(n)) => {
                drop(gate);
                Poll::Ready(Err(RecvError::Lagged(n)))
            }
            Err(inner::error::TryRecvError::Closed) => {
                drop(gate);
                Poll::Ready(Err(RecvError::Closed))
            }
            Err(inner::error::TryRecvError::Empty) => {
                if gate.closed {
                    drop(gate);
                    return Poll::Ready(Err(RecvError::Closed));
                }
                match rx.shared.backend {
                    Backend::Engine(cvid) => {
                        let (handle, adv) =
                            system::register_channel_async(cvid, cx.waker().clone());
                        rx.pending = Some(Parked::Engine(handle));
                        drop(gate);
                        adv.fire();
                    }
                    Backend::Native => {
                        trace_native_from_ambient("broadcast", "recv_park");
                        let waker = cx.waker().clone();
                        gate.wakers.push(waker.clone());
                        rx.pending = Some(Parked::Real(waker));
                        drop(gate);
                    }
                }
                Poll::Pending
            }
        }
    }
}

impl<T> Drop for Recv<'_, T> {
    /// Removes only its own waker on drop, so a send cannot wake an already-dropped future; mirrors
    /// the same drop-race guard in `mpsc` and `oneshot`.
    fn drop(&mut self) {
        match self.rx.pending.take() {
            Some(Parked::Real(waker)) => {
                self.rx
                    .shared
                    .gate
                    .lock()
                    .wakers
                    .retain(|w| !w.will_wake(&waker));
            }
            Some(Parked::Engine(handle)) => system::cancel_async_wait(&handle),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{RecvError, TryRecvError, channel};
    use crate::{
        consts, flash,
        tokio::task::{spawn, yield_now},
    };

    /// One sender fans out to several subscribers across worker threads; each
    /// must receive every message. Subscribers park on the engine waiter between
    /// sends; a lost wakeup would strand one forever (all tasks on untimed
    /// waiters, no timed waiter to advance to: a real-time hang caught by the
    /// harness timeout).
    #[kithara::test(tokio, multi_thread)]
    async fn fan_out_no_lost_wakeup() {
        flash::reset();
        // Capacity > MSGS so no subscriber lags within the run.
        let (tx, _rx0) = channel::<usize>(consts::MSGS + 1);
        let handles: Vec<_> = (0..consts::SUBS)
            .map(|_| {
                let mut rx = tx.subscribe();
                spawn(async move {
                    let mut got = Vec::new();
                    while let Ok(value) = rx.recv().await {
                        got.push(value);
                        if got.len() == consts::MSGS {
                            break;
                        }
                    }
                    got
                })
            })
            .collect();
        for i in 0..consts::MSGS {
            tx.send(i).expect("subscribers present");
        }
        for handle in handles {
            assert_eq!(
                handle.await.expect("task joined"),
                (0..consts::MSGS).collect::<Vec<_>>()
            );
        }
    }

    /// Dropping the last sender resolves a blocked receiver with `Closed` rather
    /// than parking it forever.
    #[kithara::test(tokio, multi_thread)]
    async fn drop_senders_closes_receiver() {
        flash::reset();
        let (tx, mut rx) = channel::<usize>(4);
        let waiter = spawn(async move { rx.recv().await });
        yield_now().await;
        drop(tx);
        assert_eq!(waiter.await.expect("task joined"), Err(RecvError::Closed));
    }

    /// A receiver that overflows its ring observes `Lagged` (recoverable), then
    /// resumes from the oldest still-buffered message — tokio's overflow-drop
    /// semantics, preserved by the hybrid wrapper.
    #[kithara::test(tokio, multi_thread)]
    async fn overflow_reports_lagged() {
        flash::reset();
        let (tx, mut rx) = channel::<usize>(2);
        for i in 0..5 {
            tx.send(i).expect("receiver present");
        }
        // The receiver fell behind by 3 (sent 5, ring holds 2).
        assert_eq!(rx.try_recv(), Err(TryRecvError::Lagged(3)));
        // Recovers from the oldest retained message.
        assert_eq!(rx.recv().await, Ok(3));
        assert_eq!(rx.recv().await, Ok(4));
    }
}
