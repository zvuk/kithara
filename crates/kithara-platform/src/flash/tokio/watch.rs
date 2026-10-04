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

struct State<T> {
    value: T,
    wakers: Vec<Waker>,
    closed: bool,
    receivers: usize,
    version: u64,
}

struct Shared<T> {
    backend: Backend,
    senders: Mutex<usize>,
    state: Mutex<State<T>>,
}

impl<T> Shared<T> {
    /// Called after the version bump or close mark, with the gate released.
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
            receivers: 1,
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
    /// The last sender marks the channel closed and signals under the gate, so a receiver
    /// re-checking during teardown resolves `RecvError` instead of parking forever.
    fn drop(&mut self) {
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
    /// Borrow the latest value.
    #[must_use]
    pub fn borrow(&self) -> Ref<'_, T> {
        Ref {
            guard: self.shared.state.lock(),
        }
    }

    /// Wait until every receiver has dropped.
    pub fn closed(&self) -> impl Future<Output = ()> + '_ {
        Closed {
            sender: self,
            pending: None,
        }
    }

    /// Number of live receiving handles.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.shared.state.lock().receivers
    }

    /// Replace the watched value and wake every receiver.
    ///
    /// # Errors
    /// Returns the value back when no receivers remain (matched against
    /// `tokio`'s `send` shape, which the callers map away with `.ok()`/`let _`).
    ///
    /// Bumps the version and drains the wakers while holding the gate, so a concurrent `changed`
    /// either observes the new version or is woken.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
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
        let mut state = self.shared.state.lock();
        state.receivers += 1;
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
    seen: u64,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.state.lock().receivers += 1;
        Self {
            shared: Arc::clone(&self.shared),
            seen: self.seen,
            pending: None,
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        cancel_pending(self);
        let mut state = self.shared.state.lock();
        state.receivers -= 1;
        if state.receivers == 0 {
            let drained = std::mem::take(&mut state.wakers);
            drop(state);
            self.shared.signal(drained);
        }
    }
}

struct Closed<'a, T> {
    sender: &'a Sender<T>,
    pending: Option<Parked>,
}

impl<T> Future for Closed<'_, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if let Some(Parked::Engine(handle)) = this.pending.as_ref()
            && !handle.granted()
        {
            return Poll::Pending;
        }
        cancel_wait(&this.sender.shared, &mut this.pending);
        let mut state = this.sender.shared.state.lock();
        if state.receivers == 0 {
            return Poll::Ready(());
        }
        match this.sender.shared.backend {
            Backend::Engine(cvid) => {
                let (handle, adv) = system::register_channel_async(cvid, cx.waker().clone());
                this.pending = Some(Parked::Engine(handle));
                drop(state);
                adv.fire();
            }
            Backend::Native => {
                let waker = cx.waker().clone();
                state.wakers.push(waker.clone());
                this.pending = Some(Parked::Real(waker));
            }
        }
        Poll::Pending
    }
}

impl<T> Drop for Closed<'_, T> {
    fn drop(&mut self) {
        cancel_wait(&self.sender.shared, &mut self.pending);
    }
}

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

    /// Await the next value change.
    ///
    /// # Errors
    /// [`RecvError`] once every sender has dropped, so no further change can
    /// arrive.
    pub fn changed(&mut self) -> Changed<'_, T> {
        Changed { rx: self }
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

    /// Holds the gate across the version read and the waiter registration, so a concurrent `send`
    /// is either observed here or wakes the waiter just registered.
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
        cancel_pending(self.rx);
    }
}

impl<T> Drop for Changed<'_, T> {
    fn drop(&mut self) {
        cancel_pending(self.rx);
    }
}

/// Removes only its own waker, so a signal cannot wake an already-dropped future; mirrors the same
/// guard in `broadcast` and `mpsc`.
fn cancel_pending<T>(rx: &mut Receiver<T>) {
    cancel_wait(&rx.shared, &mut rx.pending);
}

fn cancel_wait<T>(shared: &Shared<T>, pending: &mut Option<Parked>) {
    match pending.take() {
        Some(Parked::Real(waker)) => shared.state.lock().wakers.retain(|w| !w.will_wake(&waker)),
        Some(Parked::Engine(handle)) => system::cancel_async_wait(&handle),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::channel;
    use crate::{flash, tokio::task::spawn};

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
        assert_eq!(
            waiter.await.expect("task joined"),
            7,
            "a lost wakeup would strand the awaiter until the harness timeout"
        );
    }

    #[kithara::test(tokio, multi_thread)]
    async fn drop_sender_resolves_recv_error() {
        flash::reset();
        let (tx, mut rx) = channel::<u32>(0);
        let waiter = spawn(async move { rx.changed().await });
        drop(spawn(async move {
            drop(tx);
        }));
        assert_eq!(
            waiter.await.expect("task joined"),
            Err(super::RecvError),
            "the last sender's drop resolves a blocked `changed` rather than parking it"
        );
    }

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
        assert_eq!(
            waiter.await.expect("task joined"),
            2,
            "the following `changed` awaits the next change, not the seen one"
        );
    }

    #[kithara::test(tokio, multi_thread)]
    async fn receiver_count_follows_live_receivers() {
        flash::reset();
        let (tx, rx) = channel(0u32);
        assert_eq!(tx.receiver_count(), 1);
        let second = tx.subscribe();
        let another_sender = tx.clone();
        assert_eq!(tx.receiver_count(), 2, "a sender clone is not a receiver");
        let waiter = spawn(async move {
            another_sender.closed().await;
        });
        drop(rx);
        assert_eq!(tx.receiver_count(), 1);
        drop(second);
        assert_eq!(tx.receiver_count(), 0);
        waiter.await.expect("last receiver wakes sender closed");
    }

    #[kithara::test(tokio, multi_thread)]
    async fn sender_subscribe_wait_for_and_send_replace_cover_required_surface() {
        flash::reset();
        let (tx, mut rx) = channel(false);
        assert!(!tx.send_replace(true));
        assert!(*tx.borrow(), "the sender reads what it holds");
        rx.wait_for(|value| *value).await.expect("value delivered");
        assert!(*rx.borrow());

        let mut late = tx.subscribe();
        late.wait_for(|value| *value)
            .await
            .expect("current value observed");
        assert!(*late.borrow());
    }
}
