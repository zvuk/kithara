#![forbid(unsafe_code)]

use arc_swap::ArcSwapOption;

use crate::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Retired,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, GateBackend, Thread},
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
#[derive(Default)]
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

    delegate::delegate! {
        to self.cv {
            /// Current time in this gate's deadline clock.
            #[must_use]
            pub fn instant(&self) -> Instant;
            /// Wake every waiter. Call after mutating the guarded state (the wait
            /// re-checks its predicate on wake).
            pub fn notify_all(&self);
            /// Park until the next [`notify_all`](Self::notify_all). The caller holds
            /// `guard` (having re-checked its predicate under it); the lock is
            /// released for the park and re-acquired on wake.
            #[must_use]
            pub fn wait<'a>(&self, guard: MutexGuard<'a, S>) -> MutexGuard<'a, S>;
            /// Park until the next [`notify_all`](Self::notify_all) or a deadline from [`Self::instant`].
            #[must_use]
            #[call(wait_timeout)]
            pub fn wait_until<'a>(&self, guard: MutexGuard<'a, S>, deadline: Instant) -> MutexGuard<'a, S>;
        }
    }
}

/// Exclusive section whose holder may wait on other threads. A contended
/// [`lock`](Self::lock) parks on the platform condvar, so under `flash` the
/// contender does not hold the virtual clock; use [`Mutex`] for bounded sections.
#[derive(Default)]
pub struct ExclusiveGate {
    held: CondvarGate<bool>,
}

impl ExclusiveGate {
    /// Enter the section, parking while another guard holds it.
    pub fn lock(&self) -> ExclusiveGuard<'_> {
        let mut held = self.held.lock();
        while *held {
            held = self.held.wait(held);
        }
        *held = true;
        drop(held);
        ExclusiveGuard { gate: self }
    }
}

/// Holds an [`ExclusiveGate`] until dropped.
#[must_use = "the section is held only while the guard lives"]
pub struct ExclusiveGuard<'a> {
    gate: &'a ExclusiveGate,
}

impl Drop for ExclusiveGuard<'_> {
    fn drop(&mut self) {
        *self.gate.held.lock() = false;
        self.gate.held.notify_all();
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
        let deadline = self.cv.instant() + timeout;
        let mut guard = self.lock();
        loop {
            if *guard != since {
                return true;
            }
            if self.cv.instant() >= deadline {
                return false;
            }
            guard = self.cv.wait_timeout(guard, deadline);
        }
    }
}

/// Single-waiter edge gate: signal advances atomically, loads the waiter
/// lock-free, and unparks. Its timed backstop preserves sequence progress.
pub struct ThreadGate {
    waiter: ArcSwapOption<Thread>,
    state: AtomicU64,
    waiter_id: AtomicU64,
    backend: GateBackend,
    retired_waiters: Mutex<Retired<Thread>>,
}

impl Default for ThreadGate {
    fn default() -> Self {
        Self {
            backend: GateBackend::default(),
            state: AtomicU64::new(0),
            waiter: ArcSwapOption::empty(),
            retired_waiters: Mutex::new(Retired::default()),
            waiter_id: AtomicU64::new(0),
        }
    }
}

impl ThreadGate {
    /// Record a deferred edge without waking the waiter: one atomic RMW, no
    /// syscall, so a real-time thread may call it. The next wait observes it,
    /// and a [`wait_poll_timeout`](Self::wait_poll_timeout) poll observes it at
    /// its next deadline.
    pub fn post(&self) {
        self.state.fetch_or(Self::POSTED, Ordering::SeqCst);
        self.backend.post();
    }

    /// Consume a posted edge, reporting whether one was pending.
    pub fn take_posted(&self) -> bool {
        self.state.fetch_and(!Self::POSTED, Ordering::AcqRel) & Self::POSTED != 0
    }

    /// Wait for an edge, a posted edge, or the poll deadline. Use this when work
    /// can arrive by [`post`](Self::post), which wakes nothing.
    pub fn wait_poll_timeout(&self, since: u64, timeout: Duration) -> bool {
        self.wait_timeout_inner(since, timeout, true)
    }

    fn wait_timeout_inner(&self, since: u64, timeout: Duration, poll: bool) -> bool {
        self.register();
        let deadline = thread::gate_instant(&self.backend) + timeout;
        let result = loop {
            let posts = self.backend.posts();
            if self.moved(since) || self.take_posted() {
                break true;
            }
            let now = thread::gate_instant(&self.backend);
            if now >= deadline {
                break self.moved(since) || self.take_posted();
            }
            self.backend
                .park_timeout(deadline - now, poll.then_some(posts));
        };
        // A post consumes only the wait it woke: one landing after the last
        // check stays pending for the next wait.
        self.state.fetch_and(!Self::WAITING, Ordering::SeqCst);
        result
    }

    const FLAGS: u64 = Self::WAITING | Self::POSTED;
    const POSTED: u64 = 1 << 62;
    const SEQUENCE_MASK: u64 = !Self::FLAGS;
    const WAITING: u64 = 1 << 63;

    fn advance(&self) -> u64 {
        let mut current = self.state.load(Ordering::SeqCst);
        loop {
            let next = (current & Self::FLAGS) | (current.wrapping_add(1) & Self::SEQUENCE_MASK);
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

    /// Writer-held retirees make signal guard drops pure decrements; reclaim
    /// quiesced ones here, off the signal path.
    fn publish_waiter(&self) {
        let mut retired = self.retired_waiters.lock();
        if let Some(displaced) = self.waiter.swap(Some(Arc::new(thread::current()))) {
            retired.retire(displaced);
        }
    }

    fn register(&self) {
        let waiter_id = thread::current_thread_id();
        if self.waiter_id.load(Ordering::Acquire) != waiter_id || self.waiter.load().is_none() {
            self.publish_waiter();
            self.waiter_id.store(waiter_id, Ordering::Release);
        }
        self.state.fetch_or(Self::WAITING, Ordering::SeqCst);
    }

    /// Whether a signal moved the edge past `since`. Sequentially consistent:
    /// it pairs with the waiter flag [`signal`](WaitGate::signal) reads, so
    /// either the waiter sees the edge or the signaller sees the waiter.
    fn moved(&self, since: u64) -> bool {
        Self::sequence(self.state.load(Ordering::SeqCst)) != since
    }

    const fn sequence(state: u64) -> u64 {
        state & Self::SEQUENCE_MASK
    }
}

impl WaitGate for ThreadGate {
    fn current(&self) -> u64 {
        Self::sequence(self.state.load(Ordering::Acquire))
    }

    /// Advance the edge, snapshot the active waiter lock-free, and unpark it.
    fn signal(&self) {
        let previous = self.advance();
        if previous & Self::WAITING != 0 {
            let waiter = self.waiter.load();
            self.backend
                .unpark(self.waiter_id.load(Ordering::Relaxed), waiter.as_deref());
        }
    }

    fn wait_timeout(&self, since: u64, timeout: Duration) -> bool {
        self.wait_timeout_inner(since, timeout, false)
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
    fn thread_gate_signal_during_waiter_publication_is_not_lost() {
        let gate = Arc::new(ThreadGate::default());
        let seed = gate.current();
        gate.signal();
        assert!(gate.wait_timeout(seed, Duration::ZERO));

        let publication = gate.retired_waiters.lock();
        let (started_tx, started_rx) = mpsc::channel();
        let waiter_gate = Arc::clone(&gate);
        let waiter = thread::spawn_named("threadgate-register-race", move || {
            let since = waiter_gate.current();
            started_tx.send(()).expect("start waiter registration");
            waiter_gate.wait_timeout(since, Duration::from_secs(5))
        });
        started_rx
            .recv_timeout(Instant::now() + Duration::from_secs(1))
            .expect("waiter registration started");

        gate.signal();
        drop(publication);

        assert!(waiter.join().expect("registration-race waiter"));
    }

    #[kithara::test(flash(false))]
    fn thread_gate_signal_progress_does_not_wait_for_waiter_publication() {
        let gate = Arc::new(ThreadGate::default());
        let seed = gate.current();
        gate.signal();
        assert!(gate.wait_timeout(seed, Duration::ZERO));

        let publication = gate.retired_waiters.lock();
        let (started_tx, started_rx) = mpsc::channel();
        let waiter_gate = Arc::clone(&gate);
        let waiter = thread::spawn_named("threadgate-publish-waiter", move || {
            let since = waiter_gate.current();
            started_tx.send(()).expect("start waiter publication");
            waiter_gate.wait_timeout(since, Duration::from_secs(5));
        });
        started_rx
            .recv_timeout(Instant::now() + Duration::from_secs(1))
            .expect("waiter publication started");

        let before = gate.current();
        let (progress_tx, progress_rx) = mpsc::channel();
        let signaller_gate = Arc::clone(&gate);
        let signaller = thread::spawn_named("threadgate-publish-signal", move || {
            signaller_gate.signal();
            progress_tx
                .send(signaller_gate.current())
                .expect("report signal progress");
        });
        let observed = progress_rx
            .recv_timeout(Instant::now() + Duration::from_secs(1))
            .expect("signal progresses while waiter publication is held");
        assert_ne!(observed, before, "signal must advance the edge");

        drop(publication);
        signaller.join().expect("publication-race signaller");
        waiter.join().expect("publication-race waiter");
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
        drop(gate.waiter.load());
        assert_no_alloc(|| gate.signal());

        assert!(waiter.join().expect("allocation waiter thread"));
    }

    /// The scheduler's shape: a `spawn_named` thread — counted `active` by the
    /// quiescence engine for its whole life — loops `wait_timeout` while
    /// something else keeps the edge moving. `wait_timeout` returns at once
    /// whenever the edge moved, so the loop never parks; a participant that
    /// never parks never lets the engine quiesce, and the virtual clock a
    /// sleeper waits on cannot advance. The sleep below is the discriminator:
    /// if a spinning participant can freeze the clock, it never returns. While
    /// a participant spins the clock keeps real pace, so the sleep is short.
    #[kithara::test]
    fn a_spinning_gate_waiter_must_not_freeze_the_virtual_clock() {
        let gate = Arc::new(ThreadGate::default());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let waiter_gate = Arc::clone(&gate);
        let waiter_stop = Arc::clone(&stop);
        let waiter = thread::spawn_named("gate-spin-waiter", move || {
            while !waiter_stop.load(Ordering::Acquire) {
                let since = waiter_gate.current();
                waiter_gate.wait_timeout(since, Duration::from_millis(10));
            }
        });

        let signal_gate = Arc::clone(&gate);
        let signal_stop = Arc::clone(&stop);
        let signaller = thread::spawn_named("gate-spin-signaller", move || {
            while !signal_stop.load(Ordering::Acquire) {
                signal_gate.signal();
            }
        });

        thread::sleep(Duration::from_millis(100));

        stop.store(true, Ordering::Release);
        gate.signal();
        waiter.join().expect("spin waiter thread");
        signaller.join().expect("spin signaller thread");
    }

    #[cfg(feature = "flash")]
    #[kithara::test(flash(false))]
    fn a_contended_section_lets_the_clock_reach_its_holders_wait() {
        let _ambient = crate::flash::ambient_scope(true);
        let section = Arc::new(ExclusiveGate::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (contending_tx, contending_rx) = mpsc::channel();
        let (left_tx, left_rx) = mpsc::channel();

        let holder_section = Arc::clone(&section);
        let holder = thread::spawn_named("exclusive-gate-holder", move || {
            let _held = holder_section.lock();
            entered_tx.send(()).expect("report the held section");
            // Waits for the contender to exist: a spawned thread counts as
            // running until it parks, so the clock cannot pass this sleep
            // before the contender has reached the section.
            contending_rx.recv().expect("contender spawned");
            thread::sleep(Duration::from_secs(3600));
        });
        entered_rx.recv().expect("holder entered the section");

        let contender_section = Arc::clone(&section);
        let contender = thread::spawn_named("exclusive-gate-contender", move || {
            drop(contender_section.lock());
            left_tx.send(()).expect("report the contended entry");
        });
        contending_tx
            .send(())
            .expect("release the holder into its wait");

        left_rx
            .recv()
            .expect("contender entered after the holder left");
        holder.join().expect("holder thread");
        contender.join().expect("contender thread");
    }
}
