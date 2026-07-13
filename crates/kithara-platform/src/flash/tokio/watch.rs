use std::{
    future::Future,
    ops::Deref,
    panic::Location,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use parking_lot::{Mutex, MutexGuard};

pub use super::errors::{RecvError, SendError};
use crate::{
    flash::{
        diag::PrimKind,
        flash_ambient,
        ids::{Backend, trace_native_from_ambient},
        system,
    },
    sync::Arc,
};

/// Value + version + close latch, plus the off-flash parked-receiver wakers,
/// all under one mutex (the gate). The version starts at `0`; every `send`
/// increments it. A receiver remembers the version it last saw and awaits a
/// higher one.
struct State<T> {
    value: T,
    /// Off-flash real wakers for parked receivers; drained on each signal.
    wakers: Vec<Waker>,
    /// Set by the last sender's drop so a receiver re-checking after the senders
    /// are gone resolves `RecvError` instead of re-parking.
    closed: bool,
    version: u64,
}

/// Shared between both halves (not generic over wake coordination): the gated
/// state, the live sender count and the construction-latched [`Backend`].
struct Shared<T> {
    backend: Backend,
    /// Live sender handles; the last to drop closes the channel.
    senders: Mutex<usize>,
    state: Mutex<State<T>>,
}

impl<T> Shared<T> {
    /// Wake every parked receiver so each re-checks the version. Called AFTER the
    /// version bump / close mark, with the gate already released.
    fn signal(&self, drained: Vec<Waker>) {
        match self.backend {
            Backend::Engine(cvid) => system::signal_channel(cvid, true),
            Backend::Native => {
                trace_native_from_ambient("watch", "signal");
                for waker in drained {
                    waker.wake();
                }
            }
        }
    }
}

/// Create a watch channel seeded with `init` (version `0`).
#[must_use]
#[track_caller]
pub fn channel<T>(init: T) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            value: init,
            version: 0,
            closed: false,
            wakers: Vec::new(),
        }),
        senders: Mutex::new(1),
        backend: if flash_ambient() {
            let cvid = system::next_condvar_id();
            system::describe_cvid(cvid, PrimKind::Watch, Location::caller());
            Backend::Engine(cvid)
        } else {
            Backend::Native
        },
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver {
            shared,
            seen: 0,
            pending: None,
        },
    )
}

/// Sending half: replaces the watched value and wakes every receiver.
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        *self.shared.senders.lock() += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // The last sender closes the channel: mark `closed` under the gate and
        // signal, so a receiver that re-checks during teardown still resolves
        // `RecvError` rather than parking forever.
        let mut senders = self.shared.senders.lock();
        *senders -= 1;
        let last = *senders == 0;
        drop(senders);
        if last {
            let mut state = self.shared.state.lock();
            state.closed = true;
            let drained = std::mem::take(&mut state.wakers);
            drop(state);
            self.shared.signal(drained);
        }
    }
}

impl<T> Sender<T> {
    /// Replace the watched value and wake every receiver.
    ///
    /// # Errors
    /// Returns the value back when no receivers remain (matched against
    /// `tokio`'s `send` shape, which the callers map away with `.ok()`/`let _`).
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        // Bump the version and drain the wakers WHILE holding the gate so a
        // concurrent `changed` either sees the new version or is woken below.
        let mut state = self.shared.state.lock();
        state.value = value;
        state.version += 1;
        let drained = std::mem::take(&mut state.wakers);
        drop(state);
        self.shared.signal(drained);
        Ok(())
    }

    /// Replace the watched value, wake every receiver, and return the old value.
    pub fn send_replace(&self, value: T) -> T {
        let mut state = self.shared.state.lock();
        let old = std::mem::replace(&mut state.value, value);
        state.version += 1;
        let drained = std::mem::take(&mut state.wakers);
        drop(state);
        self.shared.signal(drained);
        old
    }

    /// Create a new receiver that starts from the sender's current value.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<T> {
        let state = self.shared.state.lock();
        let seen = state.version;
        drop(state);
        Receiver {
            seen,
            shared: Arc::clone(&self.shared),
            pending: None,
        }
    }
}

/// Receiving half: borrows the latest value and awaits version changes.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    pending: Option<Parked>,
    /// The version this receiver has observed; `changed` resolves once the
    /// stored version exceeds it.
    seen: u64,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            seen: self.seen,
            pending: None,
        }
    }
}

/// How a [`Changed`] parked, so re-poll and `Drop` use the matching teardown.
/// `Real` carries the waker clone for exact-entry removal on `Drop`.
enum Parked {
    Engine(system::AsyncHandle),
    Real(Waker),
}

/// Borrow guard over the latest value, holding the gate for its lifetime.
/// `Deref`s to `T`, matching `tokio::sync::watch::Ref`'s consumed surface.
pub struct Ref<'a, T> {
    guard: MutexGuard<'a, State<T>>,
}

impl<T> Deref for Ref<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard.value
    }
}

impl<T> Receiver<T> {
    /// Borrow the latest value WITHOUT marking it seen (matches `tokio::watch`:
    /// only `borrow_and_update` advances the seen version).
    #[must_use]
    pub fn borrow(&self) -> Ref<'_, T> {
        Ref {
            guard: self.shared.state.lock(),
        }
    }

    /// Borrow the latest value AND mark it seen, so a subsequent `changed`
    /// awaits the next change rather than returning at once.
    #[must_use]
    pub fn borrow_and_update(&mut self) -> Ref<'_, T> {
        let guard = self.shared.state.lock();
        self.seen = guard.version;
        Ref { guard }
    }

    /// Report whether a newer value is waiting without awaiting one, matching
    /// [`tokio::sync::watch::Receiver::has_changed`].
    ///
    /// # Errors
    /// [`RecvError`] once every sender has dropped, so no further change can
    /// arrive.
    pub fn has_changed(&self) -> Result<bool, RecvError> {
        let guard = self.shared.state.lock();
        if guard.closed {
            return Err(RecvError);
        }
        Ok(guard.version > self.seen)
    }

    /// Await the next value change.
    ///
    /// # Errors
    /// [`RecvError`] once every sender has dropped, so no further change can
    /// arrive.
    pub fn changed(&mut self) -> Changed<'_, T> {
        Changed { rx: self }
    }

    /// Await until the latest watched value satisfies `predicate`.
    ///
    /// # Errors
    /// [`RecvError`] once every sender has dropped before a matching value
    /// arrives.
    pub fn wait_for<F>(&mut self, predicate: F) -> WaitFor<'_, T, F>
    where
        F: FnMut(&T) -> bool + Unpin,
    {
        WaitFor {
            predicate,
            rx: self,
        }
    }
}

/// Future returned by [`Receiver::changed`].
pub struct Changed<'a, T> {
    rx: &'a mut Receiver<T>,
}

impl<T> Future for Changed<'_, T> {
    type Output = Result<(), RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let rx = &mut *self.get_mut().rx;
        // Engine wait resolves only when granted; a real wait re-checks the
        // version below (a spurious wake just re-parks). Clear the marker either
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
        // Hold the gate across the version read AND the registration so a
        // concurrent `send` (version bump, then signal under the same gate) is
        // either seen here or wakes the waiter we register.
        let mut state = rx.shared.state.lock();
        if state.version > rx.seen {
            rx.seen = state.version;
            drop(state);
            return Poll::Ready(Ok(()));
        }
        if state.closed {
            drop(state);
            return Poll::Ready(Err(RecvError));
        }
        match rx.shared.backend {
            Backend::Engine(cvid) => {
                let (handle, adv) = system::register_channel_async(cvid, cx.waker().clone());
                rx.pending = Some(Parked::Engine(handle));
                drop(state);
                adv.fire();
            }
            Backend::Native => {
                trace_native_from_ambient("watch", "changed_park");
                let waker = cx.waker().clone();
                state.wakers.push(waker.clone());
                rx.pending = Some(Parked::Real(waker));
                drop(state);
            }
        }
        Poll::Pending
    }
}

/// Future returned by [`Receiver::wait_for`].
pub struct WaitFor<'a, T, F> {
    rx: &'a mut Receiver<T>,
    predicate: F,
}

impl<'a, T, F> Future for WaitFor<'a, T, F>
where
    F: FnMut(&T) -> bool + Unpin,
{
    type Output = Result<(), RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let rx = &mut *this.rx;
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

        let mut state = rx.shared.state.lock();
        rx.seen = state.version;
        if (this.predicate)(&state.value) {
            drop(state);
            return Poll::Ready(Ok(()));
        }
        if state.closed {
            drop(state);
            return Poll::Ready(Err(RecvError));
        }
        match rx.shared.backend {
            Backend::Engine(cvid) => {
                let (handle, adv) = system::register_channel_async(cvid, cx.waker().clone());
                rx.pending = Some(Parked::Engine(handle));
                drop(state);
                adv.fire();
            }
            Backend::Native => {
                trace_native_from_ambient("watch", "wait_for_park");
                let waker = cx.waker().clone();
                state.wakers.push(waker.clone());
                drop(state);
                rx.pending = Some(Parked::Real(waker));
            }
        }
        Poll::Pending
    }
}

impl<T, F> Drop for WaitFor<'_, T, F> {
    fn drop(&mut self) {
        match self.rx.pending.take() {
            Some(Parked::Real(waker)) => {
                self.rx
                    .shared
                    .state
                    .lock()
                    .wakers
                    .retain(|w| !w.will_wake(&waker));
            }
            Some(Parked::Engine(handle)) => system::cancel_async_wait(&handle),
            None => {}
        }
    }
}

impl<T> Drop for Changed<'_, T> {
    fn drop(&mut self) {
        match self.rx.pending.take() {
            // Remove EXACTLY our own waker so a signal does not wake a dropped
            // future (mirrors `broadcast`/`mpsc`).
            Some(Parked::Real(waker)) => {
                self.rx
                    .shared
                    .state
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

    use super::channel;
    use crate::{flash, tokio::task::spawn};

    /// A spawned task does (trivial) work then `send`s a new value; an awaiter
    /// parks on `changed`. A lost wakeup would strand the awaiter forever (all
    /// tasks on untimed channel waiters, no timed waiter to advance to: a
    /// real-time hang caught by the harness timeout).
    #[kithara::test(tokio, multi_thread)]
    async fn changed_no_lost_wakeup() {
        flash::reset();
        let (tx, mut rx) = channel::<u32>(0);
        let waiter = spawn(async move {
            rx.changed().await.expect("sender delivered");
            *rx.borrow()
        });
        drop(spawn(async move {
            tx.send(7).expect("receiver present");
        }));
        assert_eq!(waiter.await.expect("task joined"), 7);
    }

    /// Dropping the last sender resolves a blocked `changed` with `RecvError`
    /// rather than parking it forever.
    #[kithara::test(tokio, multi_thread)]
    async fn drop_sender_resolves_recv_error() {
        flash::reset();
        let (tx, mut rx) = channel::<u32>(0);
        let waiter = spawn(async move { rx.changed().await });
        drop(spawn(async move {
            drop(tx);
        }));
        assert_eq!(waiter.await.expect("task joined"), Err(super::RecvError));
    }

    /// `borrow_and_update` marks the current version seen, so a following
    /// `changed` awaits the NEXT change instead of returning at once.
    #[kithara::test(tokio, multi_thread)]
    async fn borrow_and_update_marks_seen() {
        flash::reset();
        let (tx, mut rx) = channel::<u32>(0);
        tx.send(1).expect("receiver present");
        assert_eq!(*rx.borrow_and_update(), 1);
        let waiter = spawn(async move {
            rx.changed().await.expect("second change delivered");
            *rx.borrow()
        });
        drop(spawn(async move {
            tx.send(2).expect("receiver present");
        }));
        assert_eq!(waiter.await.expect("task joined"), 2);
    }

    #[kithara::test(tokio, multi_thread)]
    async fn sender_subscribe_wait_for_and_send_replace_cover_required_surface() {
        flash::reset();
        let (tx, mut rx) = channel(false);
        assert!(!tx.send_replace(true));
        rx.wait_for(|value| *value).await.expect("value delivered");
        assert!(*rx.borrow());

        let mut late = tx.subscribe();
        late.wait_for(|value| *value)
            .await
            .expect("current value observed");
        assert!(*late.borrow());
    }
}
