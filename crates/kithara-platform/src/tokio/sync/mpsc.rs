//! Sim-participating `tokio::sync::mpsc` under `flash-time` (native).
//!
//! A real `tokio` mpsc parks a receiver/sender on the runtime's reactor, which
//! is invisible to the quiescence engine — so the virtual clock would advance
//! across a channel handoff and race the download chain. This wrapper keeps the
//! queue under a plain mutex and uses the engine's untimed channel waiters
//! ([`sched::register_channel_async`] / [`sched::signal_channel`]) as the wake
//! mechanism, so every send→recv handoff keeps a participant `active` and the
//! clock only advances at genuine quiescence. Off the sim path the module is not
//! compiled; callers get the real `tokio` mpsc through the glob re-export.
//!
//! Lost-wakeup freedom: a receiver/sender registers its engine waiter WHILE
//! holding the queue mutex, so a concurrent producer/consumer either observes
//! the parked waiter (and signals it) or has not yet taken the mutex.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use error::{SendError, TryRecvError};
use parking_lot::Mutex;
pub use tokio_with_wasm::alias::sync::mpsc::error;

use crate::time::sim::sched;

struct Inner<T> {
    queue: VecDeque<T>,
    /// Live sender handles; the receiver observes close at `0`.
    senders: usize,
    /// Whether the single receiver is alive; senders observe close at `false`.
    receiver_alive: bool,
}

struct Shared<T> {
    inner: Mutex<Inner<T>>,
    /// Signaled when an item is enqueued (wakes the parked receiver).
    data_cvid: u64,
    /// Signaled when a dequeue frees a slot of a full bounded queue (wakes a
    /// parked sender). Unused for unbounded channels.
    space_cvid: u64,
    /// `Some(cap)` bounded, `None` unbounded.
    capacity: Option<usize>,
}

impl<T> Shared<T> {
    fn new(capacity: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                senders: 1,
                receiver_alive: true,
            }),
            data_cvid: sched::next_condvar_id(),
            space_cvid: sched::next_condvar_id(),
            capacity,
        })
    }
}

/// Create a bounded channel with room for `capacity` queued items (min 1, like
/// `tokio`). A full channel makes `send().await` park until a receive frees a slot.
#[must_use]
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Shared::new(Some(capacity.max(1)));
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver {
            shared,
            pending: None,
        },
    )
}

/// Create an unbounded channel: `send` never blocks on capacity.
#[must_use]
pub fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let shared = Shared::new(None);
    (
        UnboundedSender {
            shared: Arc::clone(&shared),
        },
        UnboundedReceiver {
            shared,
            pending: None,
        },
    )
}

fn push_unbounded<T>(shared: &Shared<T>, value: T) -> Result<(), SendError<T>> {
    let mut inner = shared.inner.lock();
    if !inner.receiver_alive {
        return Err(SendError(value));
    }
    inner.queue.push_back(value);
    drop(inner);
    sched::signal_channel(shared.data_cvid, false);
    Ok(())
}

fn poll_recv_inner<T>(
    shared: &Shared<T>,
    pending: &mut Option<sched::AsyncHandle>,
    cx: &mut Context<'_>,
) -> Poll<Option<T>> {
    if let Some(handle) = pending.as_ref() {
        if handle.granted() {
            *pending = None;
        } else {
            return Poll::Pending;
        }
    }
    let mut inner = shared.inner.lock();
    if let Some(value) = inner.queue.pop_front() {
        let bounded = shared.capacity.is_some();
        drop(inner);
        // Every pop frees one slot: wake one parked sender if any (no-op when
        // none). Signalling only on the full→not-full edge would strand the
        // other senders when the consumer drains several slots before a woken
        // sender re-pushes — a lost wakeup that deadlocks under load.
        if bounded {
            sched::signal_channel(shared.space_cvid, false);
        }
        return Poll::Ready(Some(value));
    }
    if inner.senders == 0 {
        return Poll::Ready(None);
    }
    // Register the wakeup WHILE holding the queue lock so a concurrent send
    // cannot slip its signal between this empty-check and the park.
    let (handle, adv) = sched::register_channel_async(shared.data_cvid, cx.waker().clone());
    *pending = Some(handle);
    drop(inner);
    sched::fire_advance(adv);
    Poll::Pending
}

fn try_recv_inner<T>(shared: &Shared<T>) -> Result<T, TryRecvError> {
    let mut inner = shared.inner.lock();
    let bounded = shared.capacity.is_some();
    match inner.queue.pop_front() {
        Some(value) => {
            drop(inner);
            // Wake one parked sender per freed slot (see `poll_recv_inner`).
            if bounded {
                sched::signal_channel(shared.space_cvid, false);
            }
            Ok(value)
        }
        None if inner.senders == 0 => Err(TryRecvError::Disconnected),
        None => Err(TryRecvError::Empty),
    }
}

fn close_receiver<T>(shared: &Shared<T>, pending: &mut Option<sched::AsyncHandle>) {
    shared.inner.lock().receiver_alive = false;
    // Wake every sender blocked on capacity so each observes the closed receiver.
    sched::signal_channel(shared.space_cvid, true);
    if let Some(handle) = pending.take() {
        sched::cancel_async_wait(handle);
    }
}

fn drop_sender<T>(shared: &Shared<T>) {
    let mut inner = shared.inner.lock();
    inner.senders -= 1;
    let last = inner.senders == 0;
    drop(inner);
    if last {
        // Wake the receiver so its next poll observes the closed channel (None).
        sched::signal_channel(shared.data_cvid, false);
    }
}

/// Bounded sender (clone for multi-producer).
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    /// Send `value`, awaiting capacity when the channel is full.
    pub fn send(&self, value: T) -> Send<'_, T> {
        Send {
            shared: &self.shared,
            value: Some(value),
            pending: None,
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.inner.lock().senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        drop_sender(&self.shared);
    }
}

/// Future returned by [`Sender::send`].
pub struct Send<'a, T> {
    shared: &'a Shared<T>,
    value: Option<T>,
    pending: Option<sched::AsyncHandle>,
}

// The queued value is plain data we move out via `take` — never structurally
// pinned — so the future is `Unpin` for any payload (lets `poll` use `get_mut`).
impl<T> Unpin for Send<'_, T> {}

impl<T> Future for Send<'_, T> {
    type Output = Result<(), SendError<T>>;

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
        if !inner.receiver_alive {
            drop(inner);
            return Poll::Ready(match this.value.take() {
                Some(value) => Err(SendError(value)),
                None => Ok(()),
            });
        }
        let cap = this.shared.capacity.unwrap_or(usize::MAX);
        if inner.queue.len() < cap {
            if let Some(value) = this.value.take() {
                inner.queue.push_back(value);
                drop(inner);
                sched::signal_channel(this.shared.data_cvid, false);
            }
            return Poll::Ready(Ok(()));
        }
        let (handle, adv) =
            sched::register_channel_async(this.shared.space_cvid, cx.waker().clone());
        this.pending = Some(handle);
        drop(inner);
        sched::fire_advance(adv);
        Poll::Pending
    }
}

impl<T> Drop for Send<'_, T> {
    fn drop(&mut self) {
        if let Some(handle) = self.pending.take() {
            sched::cancel_async_wait(handle);
        }
    }
}

/// Unbounded sender (clone for multi-producer); `send` never blocks.
pub struct UnboundedSender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> UnboundedSender<T> {
    /// Enqueue `value` without blocking.
    ///
    /// # Errors
    /// Returns [`SendError`] (carrying `value`) when the receiver has dropped.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        push_unbounded(&self.shared, value)
    }
}

impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        self.shared.inner.lock().senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for UnboundedSender<T> {
    fn drop(&mut self) {
        drop_sender(&self.shared);
    }
}

/// Bounded receiver (single consumer).
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    pending: Option<sched::AsyncHandle>,
}

impl<T> Receiver<T> {
    /// Receive the next value, awaiting one while the channel is open and empty.
    pub fn recv(&mut self) -> Recv<'_, T> {
        Recv {
            shared: &self.shared,
            pending: &mut self.pending,
        }
    }

    /// Poll for the next value; `Ready(None)` once the channel is closed and drained.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        poll_recv_inner(&self.shared, &mut self.pending, cx)
    }

    /// Try to receive without blocking.
    ///
    /// # Errors
    /// [`TryRecvError::Empty`] when open but empty, [`TryRecvError::Disconnected`]
    /// once all senders dropped and the queue is drained.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        try_recv_inner(&self.shared)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        close_receiver(&self.shared, &mut self.pending);
    }
}

/// Unbounded receiver (single consumer).
pub struct UnboundedReceiver<T> {
    shared: Arc<Shared<T>>,
    pending: Option<sched::AsyncHandle>,
}

impl<T> UnboundedReceiver<T> {
    /// Receive the next value, awaiting one while the channel is open and empty.
    pub fn recv(&mut self) -> Recv<'_, T> {
        Recv {
            shared: &self.shared,
            pending: &mut self.pending,
        }
    }

    /// Poll for the next value; `Ready(None)` once the channel is closed and drained.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        poll_recv_inner(&self.shared, &mut self.pending, cx)
    }

    /// Try to receive without blocking.
    ///
    /// # Errors
    /// [`TryRecvError::Empty`] when open but empty, [`TryRecvError::Disconnected`]
    /// once all senders dropped and the queue is drained.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        try_recv_inner(&self.shared)
    }
}

impl<T> Drop for UnboundedReceiver<T> {
    fn drop(&mut self) {
        close_receiver(&self.shared, &mut self.pending);
    }
}

/// Future returned by [`Receiver::recv`] / [`UnboundedReceiver::recv`].
pub struct Recv<'a, T> {
    shared: &'a Shared<T>,
    pending: &'a mut Option<sched::AsyncHandle>,
}

impl<T> Future for Recv<'_, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let this = self.get_mut();
        poll_recv_inner(this.shared, this.pending, cx)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{channel, unbounded_channel};
    use crate::{time::sim, tokio::task::spawn};

    const PRODUCERS: usize = 8;
    const PER_PRODUCER: usize = 200;

    /// Bounded channel under a multi-thread runtime: many producers fan into a
    /// single consumer across worker threads, with a capacity small enough to
    /// exercise backpressure (parked senders woken on each receive). A lost
    /// wakeup on `data_cvid` (consumer never woken) or `space_cvid` (sender
    /// never woken) would deadlock — all tasks park on untimed channel waiters,
    /// the engine has no timed waiter to advance to, and the test hangs in real
    /// time (caught by the harness timeout). Passing proves the register-under-
    /// lock handshake is lost-wakeup free both ways.
    #[kithara::test(tokio, multi_thread)]
    async fn bounded_fan_in_no_lost_wakeup() {
        sim::reset();
        let (tx, mut rx) = channel::<usize>(4);
        for p in 0..PRODUCERS {
            let tx = tx.clone();
            drop(spawn(async move {
                for i in 0..PER_PRODUCER {
                    tx.send(p * PER_PRODUCER + i).await.expect("receiver alive");
                }
            }));
        }
        drop(tx);
        let mut seen = 0usize;
        let mut sum = 0u64;
        while let Some(v) = rx.recv().await {
            seen += 1;
            sum += v as u64;
        }
        assert_eq!(seen, PRODUCERS * PER_PRODUCER);
        let n = (PRODUCERS * PER_PRODUCER) as u64;
        assert_eq!(sum, n * (n - 1) / 2);
    }

    /// Unbounded variant: senders never block, so this isolates the `data_cvid`
    /// wakeup of a consumer that races each non-blocking send.
    #[kithara::test(tokio, multi_thread)]
    async fn unbounded_fan_in_no_lost_wakeup() {
        sim::reset();
        let (tx, mut rx) = unbounded_channel::<usize>();
        for p in 0..PRODUCERS {
            let tx = tx.clone();
            drop(spawn(async move {
                for i in 0..PER_PRODUCER {
                    tx.send(p * PER_PRODUCER + i).expect("receiver alive");
                }
            }));
        }
        drop(tx);
        let mut seen = 0usize;
        while (rx.recv().await).is_some() {
            seen += 1;
        }
        assert_eq!(seen, PRODUCERS * PER_PRODUCER);
    }

    /// Sender-side close wakes a blocked-empty consumer: with all senders
    /// dropped and the queue drained, `recv` must observe `None` rather than
    /// park forever.
    #[kithara::test(tokio, multi_thread)]
    async fn drop_senders_closes_receiver() {
        sim::reset();
        let (tx, mut rx) = channel::<usize>(2);
        let handle = spawn(async move {
            tx.send(1).await.expect("alive");
            tx.send(2).await.expect("alive");
            drop(tx);
        });
        assert_eq!(rx.recv().await, Some(1));
        assert_eq!(rx.recv().await, Some(2));
        assert_eq!(rx.recv().await, None);
        handle.await.expect("producer joined");
    }
}
