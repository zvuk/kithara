use std::{
    panic::Location,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
};

use crate::native::sync::{Condvar, Mutex, MutexGuard};

/// One waiter's wake handle: a flag + condvar so the parked OS thread blocks
/// off-lock and wakes when the engine (clock jump), an `unpark`, or a
/// `signal_condvar` fires it. The flag is set under its own lock before
/// `notify_all`, so a fire that lands before the block is not lost.
pub(crate) struct Token {
    cv: Condvar,
    woken: Mutex<bool>,
}

impl Token {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            woken: Mutex::default(),
            cv: Condvar::default(),
        })
    }
    pub(crate) fn fire(&self) {
        {
            let mut g = self.woken.lock();
            *g = true;
        }
        self.cv.notify_all();
    }
    pub(crate) fn wait(&self) {
        wait_set(&self.cv, self.woken.lock());
    }
}

/// [`Token::wait`]'s loop body: `woken` is checked under the lock before every
/// block and re-checked after every wake (spurious-wake safe). The guard
/// enters as a parameter and drops on return — the same hold window as an
/// in-line loop (parameter form keeps clippy's `significant_drop_tightening`
/// from mis-suggesting an early drop inside the loop).
fn wait_set(cv: &Condvar, mut woken: MutexGuard<'_, bool>) {
    while !*woken {
        woken = cv.wait(woken);
    }
}

/// How a parked waiter is woken once the engine (or a signal) selects it.
/// Sync OS-thread waits store a `Token` (a `native::sync` flag+condvar the blocked
/// thread sits on); async-task waits store the task's `Waker`. `try_advance` /
/// `signal_condvar` collect these and fire them AFTER releasing the engine
/// `core` lock.
pub(crate) enum Wake {
    /// A blocked OS thread (`park_timeout`, `Condvar`): wake via the `Token`.
    Sync(Arc<Token>),
    /// A parked async task (`FlashSleep`, async `Notify`): the `granted` flag is
    /// set just before waking so the future's Drop can tell "I was granted an
    /// `active` slot" from "I was still parked".
    Task {
        waker: Waker,
        granted: Arc<AtomicBool>,
        /// Identity of the async task that registered this waiter — `(task id,
        /// spawn site)` captured from
        /// [`ctx::cur_async`](crate::flash::ctx::cur_async) at registration. The
        /// task releases its `active_async` slot when it parks, so it is gone from
        /// the holder map by dump time; this preserved copy is what names WHICH
        /// task is parked on this primitive. `None` when no task context was
        /// active (e.g. a non-participated `block_on`).
        task: Option<(u64, &'static Location<'static>)>,
    },
}

impl Wake {
    pub(crate) fn fire(self) {
        match self {
            Self::Sync(t) => t.fire(),
            Self::Task { waker, .. } => waker.wake(),
        }
    }

    /// True for an async-task waiter. Async tasks are counted in `active_async`
    /// by the spawn poll-wrapper (per-poll), NOT by the firer — so a fired
    /// async waiter is `mark_granted` but never bumps a counter. Sync OS-thread
    /// waiters (`Sync`) ARE bumped by the firer into `active` (their wake has
    /// real OS-scheduling latency the bump must cover).
    pub(crate) fn is_task(&self) -> bool {
        matches!(self, Self::Task { .. })
    }

    /// Mark an async-task wake as granted an `active` slot. MUST run under the
    /// engine `core` lock at the same moment the firer does `active += 1`, so a
    /// concurrent `cancel_async_wait` (which also takes the lock) sees a
    /// consistent "entry removed ⇒ granted set" and never leaks the slot.
    pub(crate) fn mark_granted_under_lock(&self) {
        if let Self::Task { granted, .. } = self {
            granted.store(true, Ordering::Release);
        }
    }

    /// Identity of the async task parked on this waiter (`(task id, spawn site)`),
    /// for the hang dump. `None` for a sync OS-thread waiter or when no task
    /// context was captured at registration.
    pub(crate) fn task(&self) -> Option<(u64, &'static Location<'static>)> {
        match self {
            Self::Task { task, .. } => *task,
            Self::Sync(_) => None,
        }
    }
}
