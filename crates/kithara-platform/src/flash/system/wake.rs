//! Waiter wake handles for the flash quiescence engine.
//!
//! A parked waiter stores either a [`Token`] (an OS-thread flag+condvar the
//! blocked thread sits on) or an async-task [`Waker`]; [`Wake`] is the union the
//! scheduler collects and fires AFTER releasing its lock.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
};

use crate::native::sync::{Condvar, Mutex};

/// One waiter's wake handle: a flag + condvar so the parked OS thread blocks
/// off-lock and wakes when the engine (clock jump), an `unpark`, or a
/// `signal_condvar` fires it. The flag is set under its own lock before
/// `notify_all`, so a fire that lands before the block is not lost.
pub(crate) struct Token {
    woken: Mutex<bool>,
    cv: Condvar,
}

impl Token {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            woken: Mutex::new(false),
            cv: Condvar::new(),
        })
    }
    pub(crate) fn fire(&self) {
        {
            let mut g = self.woken.lock_sync();
            *g = true;
        }
        self.cv.notify_all();
    }
    pub(crate) fn wait(&self) {
        let mut g = self.woken.lock_sync();
        while !*g {
            g = self.cv.wait_sync(g);
        }
    }
}

/// How a parked waiter is woken once the engine (or a signal) selects it.
/// Sync OS-thread waits store a `Token` (a `parking_lot` flag+condvar the blocked
/// thread sits on); async-task waits store the task's `Waker`. `try_advance` /
/// `signal_condvar` collect these and fire them AFTER releasing `SCHED`.
pub(crate) enum Wake {
    /// A blocked OS thread (`park_timeout`, `Condvar`): wake via the `Token`.
    Sync(Arc<Token>),
    /// A parked async task (`sleep_sim`, async `Notify`): the `granted` flag is
    /// set just before waking so the future's Drop can tell "I was granted an
    /// `active` slot" from "I was still parked".
    Task {
        waker: Waker,
        granted: Arc<AtomicBool>,
    },
}

impl Wake {
    pub(crate) fn fire(self) {
        match self {
            Self::Sync(t) => t.fire(),
            Self::Task { waker, .. } => waker.wake(),
        }
    }

    /// Mark an async-task wake as granted an `active` slot. MUST run under the
    /// `SCHED` lock at the same moment the firer does `active += 1`, so a
    /// concurrent `cancel_async_wait` (which also takes the lock) sees a
    /// consistent "entry removed ⇒ granted set" and never leaks the slot.
    pub(crate) fn mark_granted_under_lock(&self) {
        if let Self::Task { granted, .. } = self {
            granted.store(true, Ordering::Release);
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
}
