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

/// Returned by [`Semaphore::acquire_owned`] when the semaphore is closed. A
/// distinct type from `tokio`'s (whose field is private); callers only `map_err`
/// it. Never constructed on the flash path (no `close()`); present so the flash
/// surface matches the real `tokio` Semaphore the native/wasm backends expose.
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("semaphore closed")]
#[derive(derive_more::Error)]
#[error(ignore)]
pub struct AcquireError;

struct Inner {
    /// Real-wake slots for acquirers parked on permits (off the flash path);
    /// each woken (and drained) per freed permit. Empty on the engine path.
    wakers: Vec<Waker>,
    permits: usize,
}

/// `Semaphore` under `flash`. See module docs.
pub struct Semaphore {
    /// Park/wake mechanism latched ONCE at construction (see [`Backend`]); used
    /// by every acquire/release so an acquirer and a releaser on threads of
    /// different ambient still agree on the mechanism.
    backend: Backend,
    inner: Mutex<Inner>,
}

impl Semaphore {
    #[must_use]
    #[track_caller]
    pub fn new(permits: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                permits,
                wakers: Vec::new(),
            }),
            backend: if flash_ambient() {
                let cvid = system::next_condvar_id();
                system::describe_cvid(cvid, PrimKind::Semaphore, Location::caller());
                Backend::Engine(cvid)
            } else {
                Backend::Native
            },
        }
    }

    /// Acquire a permit, awaiting one while none is free. The returned permit
    /// releases its slot on `Drop`. Mirrors `tokio`'s owned acquire (the slot is
    /// tied to the `Arc`, not a borrow), the only form the workspace uses.
    #[must_use = "the permit is released as soon as it is dropped"]
    pub fn acquire_owned(self: Arc<Self>) -> AcquireOwned {
        AcquireOwned {
            sem: self,
            pending: None,
        }
    }

    /// Return one permit and wake a single parked acquirer (it re-checks the
    /// count and takes the permit). One freed permit wakes one acquirer, exactly
    /// like the `mpsc` `space` group.
    ///
    /// Takes the waker under the same lock a park uses, so the count bump and the wake stay atomic
    /// with respect to a concurrent park.
    fn release(&self) {
        let mut inner = self.inner.lock();
        inner.permits += 1;
        let waker = match self.backend {
            Backend::Engine(_) => None,
            Backend::Native if inner.wakers.is_empty() => None,
            Backend::Native => Some(inner.wakers.remove(0)),
        };
        drop(inner);
        match self.backend {
            Backend::Engine(cvid) => system::signal_channel(cvid, false),
            Backend::Native => {
                trace_native_from_ambient("semaphore", "release");
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
        }
    }
}

/// How an [`AcquireOwned`] parked, so re-poll and `Drop` use the matching
/// teardown. `Real` carries the waker clone for exact-entry removal on `Drop`.
enum Parked {
    Engine(system::AsyncHandle),
    Real(Waker),
}

/// Owned permit: returns its slot to the semaphore on `Drop`.
pub struct OwnedSemaphorePermit {
    sem: Arc<Semaphore>,
}

impl Drop for OwnedSemaphorePermit {
    fn drop(&mut self) {
        self.sem.release();
    }
}

/// Future returned by [`Semaphore::acquire_owned`].
pub struct AcquireOwned {
    sem: Arc<Semaphore>,
    pending: Option<Parked>,
}

impl Unpin for AcquireOwned {}

impl Future for AcquireOwned {
    type Output = Result<OwnedSemaphorePermit, AcquireError>;

    /// Registers the waiter while holding the count lock, so a concurrent `release` (same lock,
    /// then signal) cannot slip its wake between this check and the park.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(Parked::Engine(handle)) = this.pending.as_ref() {
            if handle.granted() {
                this.pending = None;
            } else {
                return Poll::Pending;
            }
        }
        let mut inner = this.sem.inner.lock();
        if inner.permits > 0 {
            inner.permits -= 1;
            return Poll::Ready(Ok(OwnedSemaphorePermit {
                sem: Arc::clone(&this.sem),
            }));
        }
        match this.sem.backend {
            Backend::Engine(cvid) => {
                let (handle, adv) = system::register_channel_async(cvid, cx.waker().clone());
                this.pending = Some(Parked::Engine(handle));
                drop(inner);
                adv.fire();
            }
            Backend::Native => {
                trace_native_from_ambient("semaphore", "acquire_park");
                let waker = cx.waker().clone();
                inner.wakers.push(waker.clone());
                this.pending = Some(Parked::Real(waker));
                drop(inner);
            }
        }
        Poll::Pending
    }
}

impl Drop for AcquireOwned {
    /// Removes only its own waker on drop, so a `release` cannot wake an already-dropped acquirer;
    /// mirrors the same drop-race guard in `mpsc`'s `Send::drop`.
    fn drop(&mut self) {
        match self.pending.take() {
            Some(Parked::Real(waker)) => {
                self.sem
                    .inner
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_test_utils::kithara;

    use super::Semaphore;
    use crate::{
        consts, flash,
        sync::Arc,
        tokio::task::{spawn, yield_now},
    };

    /// Many tasks contend for a few permits; each acquires, yields (forcing the
    /// others to park on the engine waiter), then releases by dropping the
    /// permit. Every task must eventually acquire — a lost wakeup on a permit
    /// release would strand a parked acquirer forever (all tasks on untimed
    /// waiters, no timed waiter to advance to: a real-time hang caught by the
    /// harness timeout).
    #[kithara::test(tokio, multi_thread)]
    async fn contention_no_lost_wakeup() {
        flash::reset();
        let sem = Arc::new(Semaphore::new(consts::PERMITS));
        let done = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..consts::TASKS)
            .map(|_| {
                let sem = Arc::clone(&sem);
                let done = Arc::clone(&done);
                spawn(async move {
                    let permit = Arc::clone(&sem).acquire_owned().await.expect("not closed");
                    yield_now().await;
                    drop(permit);
                    done.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for handle in handles {
            handle.await.expect("task joined");
        }
        assert_eq!(done.load(Ordering::SeqCst), consts::TASKS);
    }

    /// A permit dropped while an acquirer is parked must wake it: hold the sole
    /// permit, park a second acquirer, then release — the parked acquirer
    /// proceeds rather than hanging.
    #[kithara::test(tokio, multi_thread)]
    async fn release_wakes_parked_acquirer() {
        flash::reset();
        let sem = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&sem)
            .acquire_owned()
            .await
            .expect("first permit");
        let sem2 = Arc::clone(&sem);
        let waiter = spawn(async move {
            let _permit = sem2.acquire_owned().await.expect("second permit");
        });
        yield_now().await;
        drop(held);
        waiter.await.expect("waiter joined");
    }
}
