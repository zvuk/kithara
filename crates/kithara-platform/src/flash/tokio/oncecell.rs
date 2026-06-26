use std::{
    future::Future,
    pin::Pin,
    sync::OnceLock,
    task::{Context, Poll, Waker},
};

use parking_lot::Mutex;

use crate::flash::{
    flash_ambient,
    ids::{Backend, trace_native_from_ambient},
    system,
};

/// Init-coordination state, separate from the value's [`OnceLock`]: whether a
/// caller currently owns the init, plus the real-wake slots for callers parked
/// behind it (off the flash path; empty on the engine path).
#[derive(Debug, Default)]
struct Init {
    wakers: Vec<Waker>,
    in_progress: bool,
}

/// `OnceCell` under `flash`. The value lives in a [`OnceLock`] (stable `&T`);
/// concurrent `get_or_try_init` losers park on the construction-latched
/// [`Backend`] until the winner's init resolves.
pub struct OnceCell<T> {
    backend: Backend,
    init: Mutex<Init>,
    value: OnceLock<T>,
}

impl<T> OnceCell<T> {
    /// Release the init claim and wake every parked waiter so each re-checks the
    /// value (set ⇒ resolves) or contends for the next turn (init failed). On
    /// the flash path one `signal_channel(_, true)` wakes all engine waiters.
    fn finish_init(&self) {
        let mut init = self.init.lock();
        init.in_progress = false;
        let wakers = std::mem::take(&mut init.wakers);
        drop(init);
        match self.backend {
            Backend::Engine(cvid) => system::signal_channel(cvid, true),
            Backend::Native => {
                trace_native_from_ambient("oncecell", "finish_init");
                for waker in wakers {
                    waker.wake();
                }
            }
        }
    }

    /// Get the value, or run `f` to initialize it. The first caller runs `f`;
    /// concurrent callers park until it resolves, then observe the value (on
    /// `Ok`) or take a turn to retry (on `Err` — the cell stays empty, matching
    /// `tokio`).
    ///
    /// # Errors
    /// Propagates the initializer's error; the cell stays uninitialized so a
    /// later call can retry.
    pub async fn get_or_try_init<E, F, Fut>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        loop {
            // Authoritative fast check — a set value is never unset.
            if let Some(value) = self.value.get() {
                return Ok(value);
            }
            let claim = {
                let mut init = self.init.lock();
                if self.value.get().is_some() {
                    Claim::Ready
                } else if init.in_progress {
                    Claim::Wait
                } else {
                    init.in_progress = true;
                    Claim::Init
                }
            };
            match claim {
                // Re-loop: the fast check returns the now-set value.
                Claim::Ready => continue,
                // Park until the current initializer resolves, then re-loop.
                Claim::Wait => AwaitChange::new(self).await,
                Claim::Init => {
                    // We own the init. The guard releases the init (and wakes
                    // waiters) if `f().await` is cancelled or panics before we
                    // disarm it, so a waiter is never stranded.
                    let mut guard = AbandonGuard {
                        cell: self,
                        armed: true,
                    };
                    let result = f().await;
                    guard.armed = false;
                    return match result {
                        Ok(value) => {
                            // Sole initializer + empty cell ⇒ this sets it; the
                            // returned `&T` borrows the stable `OnceLock` slot.
                            let stored = self.value.get_or_init(move || value);
                            self.finish_init();
                            Ok(stored)
                        }
                        Err(err) => {
                            self.finish_init();
                            Err(err)
                        }
                    };
                }
            }
        }
    }
}

impl<T> Default for OnceCell<T> {
    fn default() -> Self {
        Self {
            value: OnceLock::new(),
            init: Mutex::new(Init::default()),
            backend: if flash_ambient() {
                Backend::Engine(system::next_condvar_id())
            } else {
                Backend::Native
            },
        }
    }
}

impl<T> std::fmt::Debug for OnceCell<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnceCell")
            .field("backend", &self.backend)
            .field("init", &self.init)
            .field("initialized", &self.value.get().is_some())
            .finish()
    }
}

/// Decision taken under the init lock for one `get_or_try_init` turn.
enum Claim {
    /// The value is already set; re-loop to return it.
    Ready,
    /// Another caller owns the init; park until it resolves.
    Wait,
    /// We claimed the init (`in_progress` set); run `f`.
    Init,
}

/// How an [`AwaitChange`] parked, so re-poll and `Drop` use the matching
/// teardown. `Real` carries the waker clone for exact-entry removal on `Drop`.
enum Parked {
    Engine(system::AsyncHandle),
    Real(Waker),
}

/// Future that parks a losing caller until the current initializer resolves.
/// Resolving means only "the state changed — re-check it"; the caller's loop
/// re-evaluates the authoritative value / `in_progress` state, so a spurious
/// wake just re-parks.
struct AwaitChange<'a, T> {
    cell: &'a OnceCell<T>,
    pending: Option<Parked>,
}

impl<'a, T> AwaitChange<'a, T> {
    fn new(cell: &'a OnceCell<T>) -> Self {
        Self {
            cell,
            pending: None,
        }
    }
}

impl<T> Unpin for AwaitChange<'_, T> {}

impl<T> Future for AwaitChange<'_, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        match this.pending.as_ref() {
            // Engine wait resolves only when granted (a clock jump via some other
            // advance cannot resolve it early).
            Some(Parked::Engine(handle)) => {
                if handle.granted() {
                    this.pending = None;
                    return Poll::Ready(());
                }
                return Poll::Pending;
            }
            // Real wait: woken by `finish_init` (or spuriously). Resolve so the
            // caller re-checks state under the lock and re-parks if still busy.
            Some(Parked::Real(_)) => {
                this.pending = None;
                return Poll::Ready(());
            }
            None => {}
        }
        // Register the waiter WHILE holding the init lock so a concurrent
        // `finish_init` (same lock, then signal) cannot slip between this
        let mut init = this.cell.init.lock();
        if this.cell.value.get().is_some() || !init.in_progress {
            return Poll::Ready(());
        }
        match this.cell.backend {
            Backend::Engine(cvid) => {
                let (handle, adv) = system::register_channel_async(cvid, cx.waker().clone());
                this.pending = Some(Parked::Engine(handle));
                drop(init);
                adv.fire();
            }
            Backend::Native => {
                trace_native_from_ambient("oncecell", "await_init");
                let waker = cx.waker().clone();
                init.wakers.push(waker.clone());
                this.pending = Some(Parked::Real(waker));
                drop(init);
            }
        }
        Poll::Pending
    }
}

impl<T> Drop for AwaitChange<'_, T> {
    fn drop(&mut self) {
        match self.pending.take() {
            // Remove EXACTLY our own waker so a `finish_init` does not wake a
            // dropped future (leaving a stale waker is harmless here — it only
            // costs one spurious wake — but exact removal keeps the list tight).
            Some(Parked::Real(waker)) => {
                self.cell
                    .init
                    .lock()
                    .wakers
                    .retain(|w| !w.will_wake(&waker));
            }
            Some(Parked::Engine(handle)) => system::cancel_async_wait(&handle),
            None => {}
        }
    }
}

/// Releases a claimed init if the initializer's future is dropped (cancelled or
/// panicked) before it finalizes, so waiters are never stranded. Disarmed once
/// `f().await` returns and the caller finalizes explicitly.
struct AbandonGuard<'a, T> {
    cell: &'a OnceCell<T>,
    armed: bool,
}

impl<T> Drop for AbandonGuard<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            self.cell.finish_init();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use kithara_test_utils::kithara;

    use super::OnceCell;
    use crate::{
        flash,
        tokio::task::{spawn, yield_now},
    };

    const WAITERS: usize = 8;

    /// Many tasks race `get_or_try_init` on a shared cell; the winning
    /// initializer yields before producing the value, so the losers park on the
    /// engine waiter. The init closure must run EXACTLY once and every caller
    /// must observe the same value. A lost wakeup would leave a loser parked
    /// forever (all tasks on untimed waiters, no timed waiter to advance to —
    /// a real-time hang caught by the harness timeout).
    #[kithara::test(tokio, multi_thread)]
    async fn concurrent_init_runs_once_no_lost_wakeup() {
        flash::reset();
        let cell: Arc<OnceCell<usize>> = Arc::new(OnceCell::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..WAITERS)
            .map(|_| {
                let cell = Arc::clone(&cell);
                let calls = Arc::clone(&calls);
                spawn(async move {
                    cell.get_or_try_init(|| async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        yield_now().await;
                        Ok::<_, ()>(42usize)
                    })
                    .await
                    .copied()
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.await.expect("task joined"), Ok(42));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A failing initializer leaves the cell empty so a later call retries and
    /// succeeds — and the failure must wake the cell's init slot rather than
    /// wedge a subsequent caller.
    #[kithara::test(tokio, multi_thread)]
    async fn failed_init_retries() {
        flash::reset();
        let cell: OnceCell<usize> = OnceCell::default();
        let first = cell.get_or_try_init(|| async { Err("boom") }).await;
        assert_eq!(first, Err("boom"));
        let second = cell.get_or_try_init(|| async { Ok::<_, &str>(7) }).await;
        assert_eq!(second, Ok(&7));
    }
}
