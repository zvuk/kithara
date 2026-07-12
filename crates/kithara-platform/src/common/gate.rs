#![forbid(unsafe_code)]

use crate::{
    sync::{
        Condvar, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
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
    cv: Condvar,
    state: Mutex<S>,
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

/// Single-waiter edge gate with a non-blocking signal path and timed backstop.
/// Sequence and waiter registration share one atomic to prevent lost wakeups.
pub struct ThreadGate {
    backend: thread::GateBackend,
    state: AtomicU64,
    waiter: Mutex<Option<Thread>>,
    waiter_id: AtomicU64,
}

impl Default for ThreadGate {
    fn default() -> Self {
        Self {
            backend: thread::GateBackend::default(),
            state: AtomicU64::new(0),
            waiter: Mutex::new(None),
            waiter_id: AtomicU64::new(0),
        }
    }
}

impl ThreadGate {
    const SEQUENCE_MASK: u64 = !Self::WAITING;
    const WAITING: u64 = 1 << 63;

    fn advance(&self) -> u64 {
        let mut current = self.state.load(Ordering::SeqCst);
        loop {
            let next = (current & Self::WAITING) | (current.wrapping_add(1) & Self::SEQUENCE_MASK);
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(previous) => return previous,
                Err(observed) => current = observed,
            }
        }
    }

    fn register(&self) {
        let waiter_id = thread::current_thread_id();
        let mut waiter = self.waiter.lock();
        if self.waiter_id.load(Ordering::Acquire) != waiter_id || waiter.is_none() {
            *waiter = Some(thread::current());
            self.waiter_id.store(waiter_id, Ordering::Release);
        }
        drop(waiter);
        self.state.fetch_or(Self::WAITING, Ordering::SeqCst);
    }

    fn sequence(state: u64) -> u64 {
        state & Self::SEQUENCE_MASK
    }
}

impl WaitGate for ThreadGate {
    fn current(&self) -> u64 {
        Self::sequence(self.state.load(Ordering::Acquire))
    }

    /// Advance the edge and attempt to unpark an active waiter without blocking.
    fn signal(&self) {
        let previous = self.advance();
        if previous & Self::WAITING != 0
            && let Ok(waiter) = self.waiter.try_lock()
        {
            self.backend
                .unpark(self.waiter_id.load(Ordering::Relaxed), waiter.as_ref());
        }
    }

    fn wait_timeout(&self, since: u64, timeout: Duration) -> bool {
        self.register();
        let deadline = thread::gate_instant(&self.backend) + timeout;
        let result = loop {
            let state = self.state.load(Ordering::SeqCst);
            if Self::sequence(state) != since {
                break true;
            }
            let now = thread::gate_instant(&self.backend);
            if now >= deadline {
                break Self::sequence(self.state.load(Ordering::SeqCst)) != since;
            }
            self.backend.park_timeout(deadline - now);
        };
        self.state.fetch_and(Self::SEQUENCE_MASK, Ordering::SeqCst);
        result
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::time::{Duration, Instant as StdInstant};

    use assert_no_alloc::assert_no_alloc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        sync::{Arc, mpsc},
        thread,
    };

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

    #[kithara::test(flash(false))]
    fn thread_gate_refreshes_waiter_handle() {
        let g = Arc::new(ThreadGate::default());

        let first_gate = Arc::clone(&g);
        let first = thread::spawn_named("threadgate-stale-waiter", move || {
            let first_id = thread::current_thread_id();
            let since = first_gate.current();
            first_gate.signal();
            assert!(first_gate.wait_timeout(since, Duration::from_millis(10)));
            first_id
        });
        let first_id = first.join().expect("first waiter thread");

        let (registered_tx, registered_rx) = mpsc::channel();

        let second_gate = Arc::clone(&g);
        let second = thread::spawn_named("threadgate-current-waiter", move || {
            let since = second_gate.current();
            let second_id = thread::current_thread_id();
            registered_tx
                .send(second_id)
                .expect("report second waiter registration");

            let started = StdInstant::now();
            let moved = second_gate.wait_timeout(since, Duration::from_millis(250));
            let elapsed = started.elapsed();
            (second_id, elapsed, moved)
        });

        let second_id = registered_rx
            .recv_timeout(Instant::now() + Duration::from_secs(1))
            .expect("second waiter registered");
        assert!(
            first_id != second_id,
            "test requires two live waiter threads"
        );

        while g.state.load(Ordering::Acquire) & ThreadGate::WAITING == 0 {
            thread::yield_now();
        }
        g.signal();

        let (_second_id, elapsed, moved) = second.join().expect("second waiter thread");
        assert!(moved, "signal must advance the gate before waking");
        assert!(
            elapsed < Duration::from_millis(200),
            "signal woke stale waiter; current waiter only returned after {elapsed:?}"
        );
    }

    #[kithara::test(flash(false))]
    fn thread_gate_signal_does_not_allocate() {
        let gate = Arc::new(ThreadGate::default());
        let waiter_gate = Arc::clone(&gate);
        let waiter = thread::spawn_named("threadgate-allocation-waiter", move || {
            let since = waiter_gate.current();
            waiter_gate.wait_timeout(since, Duration::from_secs(1))
        });

        while gate.state.load(Ordering::Acquire) & ThreadGate::WAITING == 0 {
            thread::yield_now();
        }
        assert_no_alloc(|| gate.signal());

        assert!(waiter.join().expect("allocation waiter thread"));
    }
}
