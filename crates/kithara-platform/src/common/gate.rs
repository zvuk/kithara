//! Unified wait/notify gates — one home for the project's off-RT and
//! RT-safe readiness primitives (the former `SchedulerWake`, the assets
//! `ReadinessGate`, and the storage range-wait condvar).
//!
//! Two shapes cover every constraint profile in the workspace; both are
//! flash-aware (built on `crate::sync::{Mutex, Condvar}` / `crate::thread`),
//! so a timed wait collapses the virtual clock and an untimed wait is
//! lost-wakeup-free under `flash`.
//!
//! - [`CondvarGate<S>`] — off-RT, multi-waiter. A `Mutex<S>` guarded state
//!   plus a `Condvar`. Used two ways:
//!   - **guarded-state** (`S` = the predicate itself, e.g. storage
//!     `CommonState`, the readiness `bool`): the predicate lives *inside* the
//!     gate lock, so a wait is single-lock airtight — the storage model.
//!   - **edge** (`S` = `u64` counter, external predicate): [`signal`] bumps
//!     the counter, a waiter snapshots [`current`] before checking its
//!     externally-locked predicate and parks only if the counter is
//!     unchanged. A seqlock guard that closes the lost-wakeup window without
//!     the predicate and the gate sharing a lock.
//! - [`ThreadGate`] — RT-safe wake, single-waiter. A lock-free [`signal`]
//!   (atomic bump + `unpark`, callable from the forbid-blocking audio core)
//!   plus a `park_timeout` waiter. The wake side never touches a condvar
//!   mutex; the counter bump alone covers the rare race where a `signal`
//!   lands before the waiter registers.
//!
//! ## When to pick which
//!
//! - Off-RT signaller, want pure event-driven (no timer): `CondvarGate`.
//! - Signaller runs on the RT audio core (must not lock): `ThreadGate`.

#![forbid(unsafe_code)]

use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    sync::{Condvar, Mutex, MutexGuard},
    thread::{self, Thread},
    time::{Duration, Instant},
};

/// Shared edge API: a monotonic counter ("something happened") plus a wake
/// and a bounded wait. Snapshot [`current`](Self::current) BEFORE checking
/// your (possibly externally-locked) predicate, then pass that snapshot to a
/// wait — the wait returns immediately if the counter already moved, so a
/// `signal` racing the predicate check is never lost.
pub trait WaitGate {
    /// Snapshot the edge counter.
    fn current(&self) -> u64;

    /// Bump the counter and wake every waiter.
    fn signal(&self);

    /// Block until the counter differs from `since` or `timeout` elapses.
    /// Returns `true` if the counter moved (an event landed), `false` on a
    /// pure timeout.
    fn wait_timeout(&self, since: u64, timeout: Duration) -> bool;
}

/// Off-RT condvar gate over guarded state `S`. See the module docs for the
/// guarded-state vs edge usage.
pub struct CondvarGate<S> {
    state: Mutex<S>,
    cv: Condvar,
}

impl<S> CondvarGate<S> {
    /// Construct with the initial guarded state.
    pub fn new(state: S) -> Self {
        Self {
            state: Mutex::new(state),
            cv: Condvar::default(),
        }
    }

    /// Lock the guarded state.
    pub fn lock(&self) -> MutexGuard<'_, S> {
        self.state.lock()
    }

    /// Wake every waiter. Call after mutating the guarded state (the wait
    /// re-checks its predicate on wake).
    pub fn notify_all(&self) {
        self.cv.notify_all();
    }

    /// Park until the next [`notify_all`](Self::notify_all). The caller holds
    /// `guard` (having re-checked its predicate under it); the lock is
    /// released for the park and re-acquired on wake.
    #[must_use]
    pub fn wait<'a>(&self, guard: MutexGuard<'a, S>) -> MutexGuard<'a, S> {
        self.cv.wait(guard)
    }

    /// Park until the next [`notify_all`](Self::notify_all) or `deadline`.
    #[must_use]
    pub fn wait_until<'a>(&self, guard: MutexGuard<'a, S>, deadline: Instant) -> MutexGuard<'a, S> {
        self.cv.wait_timeout(guard, deadline)
    }
}

impl<S: Default> Default for CondvarGate<S> {
    fn default() -> Self {
        Self {
            state: Mutex::new(S::default()),
            cv: Condvar::default(),
        }
    }
}

impl WaitGate for CondvarGate<u64> {
    fn current(&self) -> u64 {
        *self.lock()
    }

    fn signal(&self) {
        {
            let mut guard = self.lock();
            *guard = guard.wrapping_add(1);
        }
        self.cv.notify_all();
    }

    fn wait_timeout(&self, since: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.lock();
        loop {
            if *guard != since {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            guard = self.cv.wait_timeout(guard, deadline);
        }
    }
}

/// RT-safe edge gate: a lock-free [`signal`](WaitGate::signal) (atomic bump +
/// `unpark`) plus a `park_timeout` waiter. Single-waiter — the first
/// [`wait_timeout`](WaitGate::wait_timeout) caller registers its thread for
/// the `unpark` fast-path.
#[derive(Default)]
pub struct ThreadGate {
    seq: AtomicU64,
    waiter: OnceLock<Thread>,
}

impl ThreadGate {
    fn register(&self) {
        if self.waiter.get().is_none() {
            let _ = self.waiter.set(thread::current());
        }
    }
}

impl WaitGate for ThreadGate {
    fn current(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Lock-free: bump the counter and `unpark` the registered waiter. Safe on
    /// the RT audio core — no condvar mutex. The counter bump alone closes the
    /// lost-wakeup window (a waiter that snapshotted the old value sees the
    /// change and never parks); the `unpark` is a latency fast-path.
    fn signal(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        if let Some(waiter) = self.waiter.get() {
            thread::unpark(waiter);
        }
    }

    fn wait_timeout(&self, since: u64, timeout: Duration) -> bool {
        self.register();
        let deadline = Instant::now() + timeout;
        loop {
            if self.seq.load(Ordering::Acquire) != since {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return self.seq.load(Ordering::Acquire) != since;
            }
            thread::park_timeout(deadline - now);
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::Arc;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::thread;

    #[kithara::test]
    fn edge_signal_advances_current() {
        let g = CondvarGate::<u64>::default();
        let s0 = g.current();
        g.signal();
        assert_ne!(g.current(), s0, "signal must advance the edge counter");
    }

    #[kithara::test]
    fn edge_wait_timeout_expires_without_signal() {
        let g = CondvarGate::<u64>::default();
        let s0 = g.current();
        assert!(
            !g.wait_timeout(s0, Duration::from_millis(10)),
            "no signal: wait_timeout returns false"
        );
    }

    #[kithara::test]
    fn guarded_state_wait_until_predicate() {
        let g = Arc::new(CondvarGate::new(false));
        let setter = Arc::clone(&g);
        let join = thread::spawn_named("gate-state-set", move || {
            thread::sleep(Duration::from_millis(5));
            *setter.lock() = true;
            setter.notify_all();
        });
        let mut guard = g.lock();
        while !*guard {
            guard = g.wait(guard);
        }
        assert!(*guard);
        drop(guard);
        join.join().expect("setter thread");
    }

    #[kithara::test]
    fn thread_gate_wait_timeout_expires_without_signal() {
        let g = ThreadGate::default();
        let s0 = g.current();
        assert!(!g.wait_timeout(s0, Duration::from_millis(10)));
    }

    #[kithara::test]
    fn thread_gate_wakes_on_cross_thread_signal() {
        let g = Arc::new(ThreadGate::default());
        let s0 = g.current();
        let signaller = Arc::clone(&g);
        let join = thread::spawn_named("threadgate-signal", move || {
            thread::sleep(Duration::from_millis(5));
            signaller.signal();
        });
        assert!(
            g.wait_timeout(s0, Duration::from_secs(5)),
            "cross-thread signal must wake before the backstop"
        );
        join.join().expect("signaller thread");
    }

    #[kithara::test]
    fn thread_gate_signal_before_wait_is_not_lost() {
        let g = ThreadGate::default();
        let s0 = g.current();
        // signal lands before any wait registers the waiter thread: the
        // counter bump alone must make the next wait return immediately.
        g.signal();
        assert!(g.wait_timeout(s0, Duration::from_millis(10)));
    }
}
