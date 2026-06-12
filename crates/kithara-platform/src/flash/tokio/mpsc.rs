//! Sim-participating `tokio::sync::mpsc` under `flash` (native).
//!
//! A real `tokio` mpsc parks a receiver/sender on the runtime's reactor, which
//! is invisible to the quiescence engine — so the virtual clock would advance
//! across a channel handoff and race the download chain. This wrapper keeps the
//! queue under a plain mutex and uses the engine's untimed channel waiters
//! ([`system::register_channel_async`] / [`system::signal_channel`]) as the wake
//! mechanism, so every send→recv handoff keeps a participant `active` and the
//! clock only advances at genuine quiescence. Off the sim path the module is not
//! compiled; callers get the real `tokio` mpsc through the glob re-export.
//!
//! Lost-wakeup freedom: a receiver/sender registers its waiter (engine handle on
//! the flash path, or its real [`Waker`] in the shared state off it) WHILE
//! holding the queue mutex, so a concurrent producer/consumer either observes the
//! parked waiter (and signals/wakes it) or has not yet taken the mutex.
//!
//! Each wait/wake branches on [`flash_ambient`]: a flash-eligible test uses the
//! engine channel waiter; otherwise the parked half stores its real [`Waker`]
//! under `inner` (the single receiver's `data_waker`, each blocked sender's
//! `space_wakers`) and the peer wakes it directly — reactor-free, untouched by
//! engine accounting. The
//! queue/live-count state is UNIFIED; only the park/wake mechanism branches. The
//! real path is the only one taken until `#[kithara::flash]` annotations land.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use error::{SendError, TryRecvError};
use parking_lot::Mutex;
pub use tokio_with_wasm::alias::sync::mpsc::error;

pub use super::unbounded::UnboundedSender;
use crate::flash::{
    flash_ambient,
    ids::{CvId, trace_native_from_ambient},
    system,
};

pub(super) struct Inner<T> {
    queue: VecDeque<T>,
    /// Live sender handles; the receiver observes close at `0`.
    senders: usize,
    /// Whether the single receiver is alive; senders observe close at `false`.
    receiver_alive: bool,
    /// Real-wake slot for the single receiver (off the flash path), mirroring
    /// the engine `data` cvid. Stored under this mutex after the empty/close
    /// re-check.
    data_waker: Option<Waker>,
    /// Real-wake slots for senders blocked on capacity (off the flash path),
    /// mirroring the engine `space` cvid. Many producers can park, so this is a
    /// list; each is woken (and the slot drained) per freed slot / on receiver
    /// close.
    space_wakers: Vec<Waker>,
}

/// Pair-carrying local form of the construction latch
/// [`crate::flash::ids::Backend`] (see its contract — same latch expression,
/// same fixed-for-life semantics): an engine channel keys TWO waiter groups.
#[derive(Clone, Copy)]
enum Backend {
    Engine {
        /// Signaled when an item is enqueued (wakes the parked receiver).
        data: CvId,
        /// Signaled when a dequeue frees a slot of a full bounded queue (wakes
        /// a parked sender). Unused for unbounded channels.
        space: CvId,
    },
    Native,
}

pub(super) struct Shared<T> {
    inner: Mutex<Inner<T>>,
    /// `Some(cap)` bounded, `None` unbounded.
    capacity: Option<usize>,
    /// Park/wake mechanism latched ONCE at channel creation (see [`Backend`]):
    /// every wake/park on both halves uses it, so a sender/receiver on a thread
    /// that did not inherit the test's ambient still reaches an engine-parked peer.
    backend: Backend,
}

impl<T> Shared<T> {
    fn new(capacity: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                senders: 1,
                receiver_alive: true,
                data_waker: None,
                space_wakers: Vec::new(),
            }),
            capacity,
            backend: if flash_ambient() {
                Backend::Engine {
                    data: system::next_condvar_id(),
                    space: system::next_condvar_id(),
                }
            } else {
                Backend::Native
            },
        })
    }

    /// Register an additional live sender handle (a clone).
    pub(super) fn add_sender(&self) {
        self.inner.lock().senders += 1;
    }

    /// Wake one parked receiver: the engine `data` cvid on the flash path, else
    /// the stored real waker. `waker` is the receiver's real waker taken under
    /// the lock by the caller (already dropped); pass `None` on the flash path.
    fn wake_data(&self, waker: Option<Waker>) {
        match self.backend {
            Backend::Engine { data, .. } => system::signal_channel(data, false),
            Backend::Native => {
                trace_native_from_ambient("mpsc", "wake_data");
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
        }
    }

    /// Wake parked senders: the engine `space` cvid on the flash path (one if
    /// `!all`, every if `all`), else the real wakers the caller drained under the
    /// lock. `wakers` is empty on the flash path.
    fn wake_space(&self, all: bool, wakers: Vec<Waker>) {
        match self.backend {
            Backend::Engine { space, .. } => system::signal_channel(space, all),
            Backend::Native => {
                trace_native_from_ambient("mpsc", "wake_space");
                for w in wakers {
                    w.wake();
                }
            }
        }
    }
}

/// How a [`Recv`]/[`Send`] future parked, so re-poll and `Drop` use the matching
/// teardown — engine cancel vs. removing the stored real waker. `Real` carries a
/// clone of the waker so a dropped sender can remove EXACTLY its own entry from
/// the shared `space_wakers` list (leaving a stale waker there would steal a
/// freed-slot wake from a still-blocked sender — a lost wakeup).
enum Parked {
    Engine(system::AsyncHandle),
    Real(Waker),
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

pub(super) fn push_unbounded<T>(shared: &Shared<T>, value: T) -> Result<(), SendError<T>> {
    let mut inner = shared.inner.lock();
    if !inner.receiver_alive {
        return Err(SendError(value));
    }
    inner.queue.push_back(value);
    // Take the receiver's real waker under the same lock the receiver parks
    // under (no-op on the flash path), so push + wake are atomic w.r.t. a recv.
    let waker = inner.data_waker.take();
    drop(inner);
    shared.wake_data(waker);
    Ok(())
}

/// Drain at most one parked sender's real waker (one freed slot wakes one
/// sender). Returns the empty list on the flash path.
fn take_one_space_waker<T>(backend: Backend, inner: &mut Inner<T>) -> Vec<Waker> {
    if matches!(backend, Backend::Engine { .. }) || inner.space_wakers.is_empty() {
        Vec::new()
    } else {
        vec![inner.space_wakers.remove(0)]
    }
}

fn poll_recv_inner<T>(
    shared: &Shared<T>,
    pending: &mut Option<Parked>,
    cx: &mut Context<'_>,
) -> Poll<Option<T>> {
    // Engine wait resolves only when granted; a real wait always re-checks the
    // queue below (a spurious wake re-parks). Clear the marker either way.
    if let Some(Parked::Engine(handle)) = pending.as_ref() {
        if handle.granted() {
            *pending = None;
        } else {
            return Poll::Pending;
        }
    }
    let mut inner = shared.inner.lock();
    if let Some(value) = inner.queue.pop_front() {
        let bounded = shared.capacity.is_some();
        // Every pop frees one slot: wake one parked sender if any (no-op when
        // none). Signalling only on the full→not-full edge would strand the
        // other senders when the consumer drains several slots before a woken
        // sender re-pushes — a lost wakeup that deadlocks under load.
        let wakers = if bounded {
            take_one_space_waker(shared.backend, &mut inner)
        } else {
            Vec::new()
        };
        drop(inner);
        if bounded {
            shared.wake_space(false, wakers);
        }
        return Poll::Ready(Some(value));
    }
    if inner.senders == 0 {
        return Poll::Ready(None);
    }
    // Register the wakeup WHILE holding the queue lock so a concurrent send
    // cannot slip its signal between this empty-check and the park.
    match shared.backend {
        Backend::Engine { data, .. } => {
            let (handle, adv) = system::register_channel_async(data, cx.waker().clone());
            *pending = Some(Parked::Engine(handle));
            drop(inner);
            adv.fire();
        }
        Backend::Native => {
            trace_native_from_ambient("mpsc", "recv_park");
            let waker = cx.waker().clone();
            inner.data_waker = Some(waker.clone());
            *pending = Some(Parked::Real(waker));
            drop(inner);
        }
    }
    Poll::Pending
}

fn try_recv_inner<T>(shared: &Shared<T>) -> Result<T, TryRecvError> {
    let mut inner = shared.inner.lock();
    let bounded = shared.capacity.is_some();
    match inner.queue.pop_front() {
        Some(value) => {
            // Wake one parked sender per freed slot (see `poll_recv_inner`).
            let wakers = if bounded {
                take_one_space_waker(shared.backend, &mut inner)
            } else {
                Vec::new()
            };
            drop(inner);
            if bounded {
                shared.wake_space(false, wakers);
            }
            Ok(value)
        }
        None if inner.senders == 0 => Err(TryRecvError::Disconnected),
        None => Err(TryRecvError::Empty),
    }
}

fn close_receiver<T>(shared: &Shared<T>, pending: &mut Option<Parked>) {
    let mut inner = shared.inner.lock();
    inner.receiver_alive = false;
    // Wake every sender blocked on capacity so each observes the closed receiver.
    let wakers = std::mem::take(&mut inner.space_wakers);
    // Drop our own data waker if we parked real, so no late push wakes us.
    if matches!(pending, Some(Parked::Real(_))) {
        inner.data_waker = None;
    }
    drop(inner);
    shared.wake_space(true, wakers);
    if let Some(Parked::Engine(handle)) = pending.take() {
        system::cancel_async_wait(&handle);
    }
}

pub(super) fn drop_sender<T>(shared: &Shared<T>) {
    let mut inner = shared.inner.lock();
    inner.senders -= 1;
    let last = inner.senders == 0;
    let waker = if last { inner.data_waker.take() } else { None };
    drop(inner);
    if last {
        // Wake the receiver so its next poll observes the closed channel (None).
        shared.wake_data(waker);
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
        self.shared.add_sender();
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
    pending: Option<Parked>,
}

// The queued value is plain data we move out via `take` — never structurally
// pinned — so the future is `Unpin` for any payload (lets `poll` use `get_mut`).
impl<T> Unpin for Send<'_, T> {}

impl<T> Future for Send<'_, T> {
    type Output = Result<(), SendError<T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Engine wait resolves only when granted; a real wait always re-checks
        // capacity/alive below (a spurious wake re-parks). Clear the marker.
        if let Some(Parked::Engine(handle)) = this.pending.as_ref() {
            if handle.granted() {
                this.pending = None;
            } else {
                return Poll::Pending;
            }
        }
        let mut inner = this.shared.inner.lock();
        if !inner.receiver_alive {
            drop(inner);
            return Poll::Ready(
                this.value
                    .take()
                    .map_or_else(|| Ok(()), |value| Err(SendError(value))),
            );
        }
        let cap = this.shared.capacity.unwrap_or(usize::MAX);
        if inner.queue.len() < cap {
            if let Some(value) = this.value.take() {
                inner.queue.push_back(value);
                let waker = inner.data_waker.take();
                drop(inner);
                this.shared.wake_data(waker);
            }
            return Poll::Ready(Ok(()));
        }
        // Full: park on space. Register the waiter WHILE holding the lock so a
        // concurrent recv (which frees a slot under the same lock, then wakes)
        // cannot slip its wake between this capacity-check and the park.
        match this.shared.backend {
            Backend::Engine { space, .. } => {
                let (handle, adv) = system::register_channel_async(space, cx.waker().clone());
                this.pending = Some(Parked::Engine(handle));
                drop(inner);
                adv.fire();
            }
            Backend::Native => {
                trace_native_from_ambient("mpsc", "send_park");
                let waker = cx.waker().clone();
                inner.space_wakers.push(waker.clone());
                this.pending = Some(Parked::Real(waker));
                drop(inner);
            }
        }
        Poll::Pending
    }
}

impl<T> Drop for Send<'_, T> {
    fn drop(&mut self) {
        match self.pending.take() {
            Some(Parked::Real(waker)) => {
                // Remove EXACTLY our own waker so a freed slot does not wake a
                // dropped sender (which would steal the wake from a still-blocked
                // peer). Other blocked senders' wakers stay in place.
                let mut inner = self.shared.inner.lock();
                inner.space_wakers.retain(|w| !w.will_wake(&waker));
            }
            Some(Parked::Engine(handle)) => system::cancel_async_wait(&handle),
            None => {}
        }
    }
}

/// Bounded receiver (single consumer).
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    pending: Option<Parked>,
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
    pending: Option<Parked>,
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
    pending: &'a mut Option<Parked>,
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
    use crate::{flash, tokio::task::spawn};

    struct Consts;
    impl Consts {
        const PRODUCERS: usize = 8;
        const PER_PRODUCER: usize = 200;
    }

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
        flash::reset();
        let (tx, mut rx) = channel::<usize>(4);
        for p in 0..Consts::PRODUCERS {
            let tx = tx.clone();
            drop(spawn(async move {
                for i in 0..Consts::PER_PRODUCER {
                    tx.send(p * Consts::PER_PRODUCER + i)
                        .await
                        .expect("receiver alive");
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
        assert_eq!(seen, Consts::PRODUCERS * Consts::PER_PRODUCER);
        let n = (Consts::PRODUCERS * Consts::PER_PRODUCER) as u64;
        assert_eq!(sum, n * (n - 1) / 2);
    }

    /// Unbounded variant: senders never block, so this isolates the `data_cvid`
    /// wakeup of a consumer that races each non-blocking send.
    #[kithara::test(tokio, multi_thread)]
    async fn unbounded_fan_in_no_lost_wakeup() {
        flash::reset();
        let (tx, mut rx) = unbounded_channel::<usize>();
        for p in 0..Consts::PRODUCERS {
            let tx = tx.clone();
            drop(spawn(async move {
                for i in 0..Consts::PER_PRODUCER {
                    tx.send(p * Consts::PER_PRODUCER + i)
                        .expect("receiver alive");
                }
            }));
        }
        drop(tx);
        let mut seen = 0usize;
        while (rx.recv().await).is_some() {
            seen += 1;
        }
        assert_eq!(seen, Consts::PRODUCERS * Consts::PER_PRODUCER);
    }

    /// Sender-side close wakes a blocked-empty consumer: with all senders
    /// dropped and the queue drained, `recv` must observe `None` rather than
    /// park forever.
    #[kithara::test(tokio, multi_thread)]
    async fn drop_senders_closes_receiver() {
        flash::reset();
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
