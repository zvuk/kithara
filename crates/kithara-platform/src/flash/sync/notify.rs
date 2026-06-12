use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

use parking_lot::Mutex;

use crate::flash::{flash_ambient, ids::CvId, system};

/// Real-wake state for the off-flash path: FIFO queue of parked waiters (each a
/// grant flag + waker, mirroring the engine's `granted`) plus a pending
/// `notify_one` permit (consumed by the next `notified()`).
#[derive(Default)]
struct RealNotify {
    waiters: VecDeque<(Arc<AtomicBool>, Waker)>,
    permit: bool,
}

/// `Notify` under `flash`. Each op branches on [`flash_ambient`]: engine
/// `cvid` group when the test is flash-eligible, else a real waker list + permit.
pub struct Notify {
    cvid: CvId,
    real: Mutex<RealNotify>,
    /// Mechanism captured ONCE at construction (see [`super::Condvar`]): every
    /// `notify_one` AND `notified()` poll uses it, so a notifier on a thread that
    /// did not inherit the test's ambient still reaches an engine-parked waiter.
    engine: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            cvid: system::next_condvar_id(),
            real: Mutex::new(RealNotify::default()),
            engine: flash_ambient(),
        }
    }
}

impl Notify {
    pub fn notify_one(&self) {
        if self.engine {
            system::signal_notify(self.cvid);
        } else {
            // Grant + wake one parked waiter; if none, store a permit (tokio
            // semantics). The grant is set UNDER the lock, atomically with the
            // pop (mirroring the engine's `mark_granted_under_lock`): a
            // concurrent `Notified::drop` then either still sees us queued, or
            // sees `granted == true` under the same lock and re-issues — it can
            // never observe a popped-but-ungranted waiter and silently drop the
            // notify. Wake after the unlock; the woken future observes its grant
            // on the next poll and resolves (never re-parks), so the notify is
            // also not lost to a spurious re-poll.
            let mut real = self.real.lock();
            let waiter = real.waiters.pop_front();
            match &waiter {
                Some((granted, _)) => granted.store(true, Ordering::Release),
                None => real.permit = true,
            }
            drop(real);
            if let Some((_, waker)) = waiter {
                waker.wake();
            }
        }
    }

    pub fn notified(&self) -> Notified<'_> {
        Notified {
            cvid: self.cvid,
            state: NotifiedState::Init,
            notify: self,
        }
    }
}

/// Park state of a [`Notified`] future, so re-poll and `Drop` use the matching
/// teardown. `Real` carries this future's grant flag so re-poll resolves on a
/// grant (never re-parks past a `notify_one`) and `Drop` removes EXACTLY its own
/// entry from the shared waiter queue (leaving a stale one would steal a
/// `notify_one` from a still-parked waiter — a lost wakeup).
enum NotifiedState {
    Init,
    Engine(system::AsyncHandle),
    Real(Arc<AtomicBool>),
    Done,
}

/// Future returned by [`Notify::notified`] under `flash`.
pub struct Notified<'a> {
    cvid: CvId,
    state: NotifiedState,
    notify: &'a Notify,
}

impl Future for Notified<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match &self.state {
            NotifiedState::Done => return Poll::Ready(()),
            NotifiedState::Engine(handle) => {
                if handle.granted() {
                    // The firer (notify_*) selected us; resolve. The task's
                    // `active_async` count is owned by the spawn poll-wrapper, so
                    // resolve touches no counter.
                    self.state = NotifiedState::Done;
                    return Poll::Ready(());
                }
                // Spurious re-poll before a signal: stay parked (engine never
                // wakes an untimed waiter on a clock jump).
                return Poll::Pending;
            }
            NotifiedState::Real(granted) => {
                if granted.load(Ordering::Acquire) {
                    // A notify_one selected us; resolve (grant-driven, never a
                    // bare permit re-check, so a spurious re-poll cannot lose it).
                    self.state = NotifiedState::Done;
                    return Poll::Ready(());
                }
                // Spurious re-poll before a grant: stay parked.
                return Poll::Pending;
            }
            NotifiedState::Init => {}
        }
        if self.notify.engine {
            let (handle, adv) = system::register_notify_async(self.cvid, cx.waker().clone());
            match handle {
                None => {
                    // A notify_one had landed with no waiter: consume the permit
                    // and resolve at once, without parking (no slot was granted).
                    self.state = NotifiedState::Done;
                    Poll::Ready(())
                }
                Some(handle) => {
                    self.state = NotifiedState::Engine(handle);
                    adv.fire();
                    Poll::Pending
                }
            }
        } else {
            // Consume a permit, else enqueue our (grant, waker) UNDER the lock so
            // a concurrent notify_one (same lock, then grant+wake) cannot slip
            // between this permit-check and our park.
            let mut real = self.notify.real.lock();
            if real.permit {
                real.permit = false;
                drop(real);
                self.state = NotifiedState::Done;
                return Poll::Ready(());
            }
            let granted = Arc::new(AtomicBool::new(false));
            real.waiters
                .push_back((Arc::clone(&granted), cx.waker().clone()));
            drop(real);
            self.state = NotifiedState::Real(granted);
            Poll::Pending
        }
    }
}

impl Drop for Notified<'_> {
    fn drop(&mut self) {
        match std::mem::replace(&mut self.state, NotifiedState::Done) {
            NotifiedState::Engine(handle) => system::cancel_async_wait(&handle),
            NotifiedState::Real(granted) => {
                // Remove EXACTLY our own entry (by grant-flag identity) so a
                // notify_one does not select a dropped future (which would steal
                // it from a still-parked peer). If our grant was already set, the
                // notify_one we consumed must be re-issued so it is not lost.
                let mut real = self.notify.real.lock();
                let before = real.waiters.len();
                real.waiters.retain(|(g, _)| !Arc::ptr_eq(g, &granted));
                let still_queued = real.waiters.len() != before;
                if !still_queued && granted.load(Ordering::Acquire) {
                    // We were granted then dropped before observing it: hand the
                    // notify on to the next waiter, or store a permit.
                    if let Some((g, w)) = real.waiters.pop_front() {
                        g.store(true, Ordering::Release);
                        drop(real);
                        w.wake();
                    } else {
                        real.permit = true;
                    }
                }
            }
            NotifiedState::Init | NotifiedState::Done => {}
        }
    }
}
