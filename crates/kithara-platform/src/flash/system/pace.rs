use std::sync::{Once, Weak};

use super::{FLASH, FlashInner};
use crate::common::time::Instant as RealInstant;

/// Per-instance lazily-spawned pacer thread. The pacer is woken lock-free via
/// `Thread::unpark` from under `core` using `sched.pacer_wake`.
pub(super) struct Pacer {
    /// One-shot lazy spawn of the pacer thread, on the first arm
    /// ([`FlashInner::real_io_enter`]).
    spawn: Once,
    /// Weak self-reference installed by `Arc::new_cyclic` at construction; the
    /// spawn site upgrades it ONCE to hand the eternal pacer thread a strong
    /// `Arc` to its OWN instance (a `&self` method cannot mint an `Arc`).
    owner: Weak<FlashInner>,
}

impl Pacer {
    pub(super) fn new(owner: Weak<FlashInner>) -> Self {
        Self {
            owner,
            spawn: Once::new(),
        }
    }

    pub(super) fn owner(&self) -> Weak<FlashInner> {
        Weak::clone(&self.owner)
    }
}

impl FlashInner {
    /// Eternal pacer loop, run on the raw pacer thread holding a strong `Arc`
    /// to this instance. Untimed parks are deliberate: when there is no paced
    /// target the pacer consumes zero CPU until the scheduler unparks it.
    fn pace_run(&self) {
        {
            let mut s = self.core.lock();
            s.sched.pacer_wake = Some(std::thread::current());
        }
        loop {
            let target = {
                let s = self.core.lock();
                s.pace_target(&self.clock)
            };
            match target {
                None => std::thread::park(),
                Some(d) => std::thread::park_timeout(d),
            }
            let mut s = self.core.lock();
            #[cfg(test)]
            {
                s.sched.pacer_wake_count += 1;
            }
            let adv = s.try_advance(&self.clock);
            drop(s);
            adv.fire();
        }
    }

    /// Mark ONE real I/O operation in flight. The first op anchors the pace to
    /// the current (real, virtual) instant and spawns the pacer thread lazily.
    ///
    /// Spawns the pacer with a raw `std::thread`, never `spawn_named`, so the pacer itself stays
    /// invisible to the engine and does not pin the clock it exists to advance.
    pub(in crate::flash) fn real_io_enter(&self) {
        self.pacer.spawn.call_once(|| {
            let owner = self
                .pacer
                .owner
                .upgrade()
                .expect("BUG: real_io_enter is reachable only through a live Arc<FlashInner>");
            std::thread::Builder::new()
                .name("kithara-flash-io-pacer".into())
                .spawn(move || owner.pace_run())
                .expect("BUG: spawning the flash io-pacer thread cannot fail");
        });
        let mut s = self.core.lock();
        s.sched.real_io += 1;
        if s.sched.real_io == 1 {
            s.sched.pace_anchor = Some((RealInstant::now(), self.clock.now_nanos()));
        }
    }

    /// Complete ONE real I/O operation. The last completion clears the anchor
    /// and immediately re-runs the advance rule: full-speed collapse resumes.
    pub(in crate::flash) fn real_io_exit(&self) {
        let mut s = self.core.lock();
        debug_assert!(s.sched.real_io > 0, "real_io exit without a matching enter");
        s.sched.real_io = s.sched.real_io.saturating_sub(1);
        if s.sched.real_io != 0 {
            return;
        }
        s.sched.pace_anchor = None;
        let adv = s.try_advance(&self.clock);
        drop(s);
        adv.fire();
    }
}

/// Process-engine forward of [`FlashInner::real_io_enter`].
pub(in crate::flash) fn real_io_enter() {
    FLASH.real_io_enter();
}

/// Process-engine forward of [`FlashInner::real_io_exit`].
pub(in crate::flash) fn real_io_exit() {
    FLASH.real_io_exit();
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Mutex, MutexGuard, PoisonError, mpsc},
        thread,
        time::Instant as RealInstant,
    };

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        flash::{
            Duration,
            system::{credit, entry::ParkRole},
        },
        sync::Arc,
    };

    fn ms(n: u64) -> u64 {
        n * 1_000_000
    }

    static GUARD: Mutex<()> = Mutex::new(());

    impl FlashInner {
        fn pacer_wake_count(&self) -> usize {
            self.core.lock().sched.pacer_wake_count
        }

        fn pacer_wake_published(&self) -> bool {
            self.core.lock().sched.pacer_wake.is_some()
        }
    }

    fn guard() -> MutexGuard<'static, ()> {
        GUARD.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bracketed_on<F: FnOnce()>(flash: &FlashInner, body: F) {
        credit::reset_credit();
        flash.pre_count_dedicated();
        credit::mark_dedicated(std::panic::Location::caller());
        body();
        flash.on_participant_exit();
    }

    /// The handle carries the park's own span. A caller that starts its clock
    /// before `spawn` and stops it after `join` charges the park with thread
    /// lifecycle, which on a loaded host is tens of milliseconds of scheduling
    /// latency the engine never spent.
    fn spawn_park_for(flash: &Arc<FlashInner>, duration: Duration) -> thread::JoinHandle<Duration> {
        let flash = Arc::clone(flash);
        thread::spawn(move || {
            let mut parked = Duration::ZERO;
            bracketed_on(&flash, || {
                let started = RealInstant::now();
                flash.park_for(duration);
                parked = started.elapsed();
            });
            parked
        })
    }

    fn wait_until(mut ready: impl FnMut() -> bool, message: &str) {
        let start = RealInstant::now();
        while !ready() {
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timed out waiting for {message}"
            );
            thread::yield_now();
        }
    }

    fn wait_for_timed_count(flash: &FlashInner, count: usize) {
        wait_until(|| flash.timed_count() == count, "timed waiter count");
    }

    fn assert_paced_elapsed(elapsed: Duration, target_ms: u64) {
        let lower = Duration::from_millis(target_ms.saturating_sub(10));
        assert!(
            elapsed >= lower,
            "paced deadline fired too early for {target_ms}ms target: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "paced deadline did not fire promptly for {target_ms}ms target: {elapsed:?}"
        );
    }

    fn pace_due(flash: &FlashInner, base: u64) {
        let mut core = flash.core.lock();
        core.sched.real_io = 1;
        core.sched.pace_anchor = Some((RealInstant::now() - Duration::from_secs(1), base));
    }

    #[kithara::test(native, flash(false))]
    fn backstops_preserve_an_already_paid_deadline() {
        for (poll, changed) in [(false, false), (true, false), (true, true)] {
            for register_other in [false, true] {
                let flash = FlashInner::new_arc();
                let base = flash.clock.now_nanos();
                let hold = flash.test_hold();
                let role = if poll {
                    ParkRole::Poll(flash.clock.posts())
                } else {
                    ParkRole::Backstop
                };
                if changed {
                    flash.clock.post();
                }
                let (paid, advance) =
                    flash.register_sleep_async(ms(500), std::task::Waker::noop().clone());
                advance.fire();
                pace_due(&flash, base);
                let anchor = flash.core.lock().sched.pace_anchor;
                let mut registered_anchor = anchor;

                if register_other {
                    let (key_tx, key_rx) = mpsc::channel();
                    let other = {
                        let flash = Arc::clone(&flash);
                        thread::spawn(move || {
                            bracketed_on(&flash, || {
                                let key = credit::current_thread_key();
                                key_tx.send(key).expect("send waiter key");
                                flash.park_timed_unparkable(Duration::from_millis(1), key, role);
                            });
                        })
                    };
                    let key = key_rx.recv().expect("receive waiter key");
                    wait_for_timed_count(&flash, 2);
                    registered_anchor = flash.core.lock().sched.pace_anchor;
                    flash.unpark(key);
                    other.join().expect("backstop waiter joins");
                }

                let remaining_waiters = flash.timed_count();
                let remaining_credit = flash.active_count();
                drop(hold);
                assert_eq!(registered_anchor, anchor);
                assert_eq!(remaining_waiters, 1);
                assert_eq!(remaining_credit, 1);
                assert!(
                    paid.granted(),
                    "{role:?}, registered backstop: {register_other}"
                );
                assert_eq!(flash.advance_log(), vec![base + ms(500)]);
                drop(paid);
                assert_eq!(flash.active_count(), 0);
                assert_eq!(flash.timed_count(), 0);
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_fresh_deadline_does_not_inherit_a_pending_timers_old_time() {
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let hold = flash.test_hold();
        let waker = std::task::Waker::noop().clone();
        let (paid, advance) = flash.register_sleep_async(ms(500), waker.clone());
        advance.fire();
        let anchor = Some((RealInstant::now() - Duration::from_secs(37), base));
        {
            let mut core = flash.core.lock();
            core.sched.real_io = 1;
            core.sched.pace_anchor = anchor;
        }

        let (fresh, advance) = flash.register_sleep_async(ms(30_000), waker);
        advance.fire();
        let fresh_anchor = flash.core.lock().sched.pace_anchor;
        drop(hold);
        drop(paid);
        let granted = fresh.granted();
        drop(fresh);

        assert_ne!(fresh_anchor, anchor);
        assert!(
            !granted,
            "a fresh deadline inherited time before its registration"
        );
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.timed_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn an_unparkable_deadline_bounds_time_banked_before_registration() {
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let hold = flash.test_hold();
        let (paid, advance) = flash.register_sleep_async(ms(500), std::task::Waker::noop().clone());
        advance.fire();
        let anchor = Some((RealInstant::now() - Duration::from_secs(37), base));
        {
            let mut core = flash.core.lock();
            core.sched.real_io = 1;
            core.sched.pace_anchor = anchor;
        }

        let (key_tx, key_rx) = mpsc::channel();
        let waiter = {
            let flash = Arc::clone(&flash);
            thread::spawn(move || {
                bracketed_on(&flash, || {
                    let key = credit::current_thread_key();
                    key_tx.send(key).expect("send waiter key");
                    flash.park_timed_unparkable(Duration::from_secs(30), key, ParkRole::Deadline);
                });
            })
        };
        let key = key_rx.recv().expect("receive waiter key");
        wait_for_timed_count(&flash, 2);
        let fresh_anchor = flash.core.lock().sched.pace_anchor;
        flash.unpark(key);
        waiter.join().expect("deadline waiter joins");
        drop(hold);
        drop(paid);

        assert_ne!(fresh_anchor, anchor);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.timed_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn a_paced_async_grant_holds_the_clock_through_poll_entry() {
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let hold = flash.test_hold();
        let gate = flash.async_acquire(std::panic::Location::caller());
        let waker = std::task::Waker::noop().clone();
        let (near, advance) = flash.register_sleep_async(ms(100), waker.clone());
        advance.fire();
        let (far, advance) = flash.register_sleep_async(ms(200), waker);
        advance.fire();
        pace_due(&flash, base);

        drop(hold);
        assert!(near.granted());
        assert!(!far.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.advance_log(), vec![base + ms(100)]);

        assert!(gate.try_enter_poll());
        assert!(!far.granted());
        assert_eq!(flash.clock.now_nanos(), base + ms(100));

        drop(near);
        assert!(far.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.advance_log(), vec![base + ms(100), base + ms(200)]);

        drop(far);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.timed_count(), 0);
        assert_eq!(flash.async_active_count(), 1);
    }

    #[kithara::test(native, flash(false))]
    fn equal_async_deadlines_share_one_grant_batch() {
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let hold = flash.test_hold();
        let waker = std::task::Waker::noop().clone();
        let (first, advance) = flash.register_sleep_async(ms(100), waker.clone());
        advance.fire();
        let (equal, advance) = flash.register_sleep_async(ms(100), waker.clone());
        advance.fire();
        let (later, advance) = flash.register_sleep_async(ms(200), waker);
        advance.fire();
        pace_due(&flash, base);

        drop(hold);
        assert!(first.granted());
        assert!(equal.granted());
        assert!(!later.granted());
        assert_eq!(flash.active_count(), 2);
        assert_eq!(flash.advance_log(), vec![base + ms(100)]);

        drop(first);
        assert!(!later.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.advance_log(), vec![base + ms(100)]);

        drop(equal);
        assert!(later.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.advance_log(), vec![base + ms(100), base + ms(200)]);

        drop(later);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.timed_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn cancelling_an_ungranted_async_wait_preserves_other_credits() {
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let hold = flash.test_hold();
        let waker = std::task::Waker::noop().clone();
        let (cancelled, advance) = flash.register_sleep_async(ms(100), waker.clone());
        advance.fire();
        let (later, advance) = flash.register_sleep_async(ms(200), waker);
        advance.fire();

        assert!(!cancelled.granted());
        drop(cancelled);
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.timed_count(), 1);
        pace_due(&flash, base);

        drop(hold);
        assert!(later.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.advance_log(), vec![base + ms(200)]);

        drop(later);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.timed_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn cancelling_a_grant_before_its_wake_fires_returns_one_credit() {
        let flash = FlashInner::new_arc();
        let (receipt, advance) = flash.register_sleep_async(0, std::task::Waker::noop().clone());

        assert!(receipt.granted());
        assert_eq!(flash.active_count(), 1);
        drop(receipt);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.timed_count(), 0);

        advance.fire();
        assert_eq!(flash.active_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn notify_and_channel_grants_share_receipt_settlement() {
        let flash = FlashInner::new_arc();
        let waker = std::task::Waker::noop().clone();
        let notify_id = flash.next_condvar_id();
        let (notify, advance) = flash.register_notify_async(notify_id, waker.clone());
        advance.fire();
        let notify = notify.expect("fresh notify has no stored permit");
        assert!(!notify.granted());

        flash.signal_notify(notify_id);
        assert!(notify.granted());
        assert_eq!(flash.active_count(), 1);
        drop(notify);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.indef_count(), 0);

        let channel_id = flash.next_condvar_id();
        let (channel, advance) = flash.register_channel_async(channel_id, waker.clone());
        advance.fire();
        assert!(!channel.granted());
        flash.signal_channel(channel_id, true);
        flash.signal_channel(channel_id, true);
        assert!(channel.granted());
        assert_eq!(flash.active_count(), 1);
        drop(channel);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.indef_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn a_receipt_cannot_publish_a_grant_after_its_engine_is_gone() {
        let receipt = {
            let flash = FlashInner::new_arc();
            let (receipt, advance) =
                flash.register_sleep_async(0, std::task::Waker::noop().clone());
            advance.fire();
            assert!(receipt.granted());
            receipt
        };

        assert!(!receipt.granted());
        drop(receipt);
    }

    #[kithara::test(native, flash(false))]
    fn known_task_grants_keep_equal_deadlines_together_through_poll_entry() {
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let hold = flash.test_hold();
        let gate = flash.async_acquire(std::panic::Location::caller());
        let diag = gate.diag();
        assert!(gate.try_enter_poll());
        let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
        let waker = std::task::Waker::noop().clone();
        let (first, advance) = flash.register_sleep_async(ms(100), waker.clone());
        advance.fire();
        let (equal, advance) = flash.register_sleep_async(ms(100), waker.clone());
        advance.fire();
        let (later, advance) = flash.register_sleep_async(ms(200), waker);
        advance.fire();
        drop(poll);
        assert!(matches!(
            flash.gate_park(&diag.state, gate.id()),
            super::super::state::ParkOutcome::Parked
        ));
        pace_due(&flash, base);

        drop(hold);
        assert!(first.granted());
        assert!(equal.granted());
        assert!(!later.granted());
        assert_eq!(flash.active_count(), 2);
        assert_eq!(flash.core.lock().registry.active, 0);
        assert_eq!(flash.advance_log(), vec![base + ms(100)]);
        assert!(matches!(
            flash.gate_wake_parked(&diag.state, gate.id(), gate.loc()),
            super::super::state::WakeOutcome::Resumed
        ));
        assert!(gate.try_enter_poll());

        drop(first);
        assert!(!later.granted());
        assert_eq!(flash.active_count(), 1);
        drop(equal);
        assert!(later.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.core.lock().registry.active, 0);
        assert_eq!(flash.advance_log(), vec![base + ms(100), base + ms(200)]);
        drop(later);
        assert_eq!(flash.active_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn grants_drain_to_a_sole_pollers_bridge_and_repin_when_it_resumes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let _runtime = runtime.enter();

        for runnable in [false, true] {
            let flash = FlashInner::new_arc();
            let base = flash.clock.now_nanos();
            let hold = flash.test_hold();
            let gate = flash.async_acquire(std::panic::Location::caller());
            let diag = gate.diag();
            assert!(gate.try_enter_poll());
            let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
            let waker = std::task::Waker::noop().clone();
            let (first, advance) = flash.register_sleep_async(ms(100), waker.clone());
            advance.fire();
            let (equal, advance) = flash.register_sleep_async(ms(100), waker.clone());
            advance.fire();
            let (later, advance) = flash.register_sleep_async(ms(200), waker.clone());
            advance.fire();
            drop(poll);
            assert!(matches!(
                flash.gate_park(&diag.state, gate.id()),
                super::super::state::ParkOutcome::Parked
            ));
            if runnable {
                assert!(matches!(
                    flash.gate_wake_parked(&diag.state, gate.id(), gate.loc()),
                    super::super::state::WakeOutcome::Resumed
                ));
            }

            let driver = flash.async_acquire(std::panic::Location::caller());
            assert!(driver.try_enter_poll());
            let poll = credit::AsyncPollGuard::enter(driver.id(), driver.loc());
            let bridge_id = flash.next_condvar_id();
            let (token, advance, wait) = flash.register_condvar_timed(base + ms(300), bridge_id);
            advance.fire();
            pace_due(&flash, base);
            drop(hold);

            assert!(first.granted());
            assert!(equal.granted());
            assert!(later.granted());
            assert_eq!(flash.clock.now_nanos(), base + ms(300));
            assert_eq!(flash.active_count(), 4);
            assert_eq!(flash.core.lock().registry.active, 1);
            assert_eq!(
                flash.advance_log(),
                vec![base + ms(100), base + ms(200), base + ms(300)]
            );
            token.wait();
            wait.resume();
            assert_eq!(flash.active_count(), 3);
            assert_eq!(flash.core.lock().registry.active, 0);

            let (after_bridge, advance) = flash.register_sleep_async(ms(100), waker);
            advance.fire();
            pace_due(&flash, base);
            assert!(!after_bridge.granted());
            drop(first);
            assert!(!after_bridge.granted());
            drop(equal);
            assert!(!after_bridge.granted());
            drop(later);
            assert!(after_bridge.granted());
            assert_eq!(flash.clock.now_nanos(), base + ms(400));
            assert_eq!(flash.active_count(), 1);
            drop(after_bridge);
            assert_eq!(flash.active_count(), 0);
            drop(poll);
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_bridged_worker_does_not_exempt_grants_another_worker_can_consume() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .expect("build multithread runtime");
        let _runtime = runtime.enter();

        for runnable in [false, true] {
            let flash = FlashInner::new_arc();
            let base = flash.clock.now_nanos();
            let hold = flash.test_hold();
            let gate = flash.async_acquire(std::panic::Location::caller());
            let diag = gate.diag();
            assert!(gate.try_enter_poll());
            let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
            let (receipt, advance) =
                flash.register_sleep_async(ms(100), std::task::Waker::noop().clone());
            advance.fire();
            drop(poll);
            assert!(matches!(
                flash.gate_park(&diag.state, gate.id()),
                super::super::state::ParkOutcome::Parked
            ));
            if runnable {
                assert!(matches!(
                    flash.gate_wake_parked(&diag.state, gate.id(), gate.loc()),
                    super::super::state::WakeOutcome::Resumed
                ));
            }

            let driver = flash.async_acquire(std::panic::Location::caller());
            assert!(driver.try_enter_poll());
            let poll = credit::AsyncPollGuard::enter(driver.id(), driver.loc());
            let bridge_id = flash.next_condvar_id();
            let (token, advance, wait) = flash.register_condvar_timed(base + ms(300), bridge_id);
            advance.fire();
            pace_due(&flash, base);
            drop(hold);

            assert!(receipt.granted());
            assert_eq!(flash.clock.now_nanos(), base + ms(100));
            assert_eq!(flash.active_count(), 1);
            assert_eq!(flash.timed_count(), 1);
            drop(receipt);
            assert_eq!(flash.clock.now_nanos(), base + ms(300));
            token.wait();
            wait.resume();
            assert_eq!(flash.active_count(), 0);
            drop(poll);
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_running_tasks_own_bridge_releases_grants_on_any_runtime() {
        for sole_poller in [false, true] {
            let mut builder = if sole_poller {
                tokio::runtime::Builder::new_current_thread()
            } else {
                tokio::runtime::Builder::new_multi_thread()
            };
            let runtime = builder.worker_threads(1).build().expect("build runtime");
            let _runtime = runtime.enter();

            for notified in [false, true] {
                let flash = FlashInner::new_arc();
                let base = flash.clock.now_nanos();
                let hold = flash.test_hold();
                let gate = flash.async_acquire(std::panic::Location::caller());
                assert!(gate.try_enter_poll());
                let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
                let notify_id = flash.next_condvar_id();
                let (receipt, advance) =
                    flash.register_notify_async(notify_id, std::task::Waker::noop().clone());
                advance.fire();
                let receipt = receipt.expect("fresh notify has no stored permit");
                flash.signal_notify(notify_id);
                assert!(receipt.granted());
                if notified {
                    std::task::Wake::wake_by_ref(&gate);
                }

                let bridge_id = flash.next_condvar_id();
                let (token, advance, wait) =
                    flash.register_condvar_timed(base + ms(100), bridge_id);
                advance.fire();
                assert_eq!(flash.clock.now_nanos(), base);
                pace_due(&flash, base);
                drop(hold);
                assert_eq!(flash.clock.now_nanos(), base + ms(100));
                assert_eq!(flash.active_count(), 2);
                token.wait();
                wait.resume();
                assert_eq!(flash.active_count(), 1);

                let (later, advance) =
                    flash.register_sleep_async(ms(100), std::task::Waker::noop().clone());
                advance.fire();
                pace_due(&flash, base);
                {
                    let mut core = flash.core.lock();
                    let advance = core.try_advance(&flash.clock);
                    drop(core);
                    advance.fire();
                }
                assert!(!later.granted());
                assert_eq!(flash.clock.now_nanos(), base + ms(100));
                drop(receipt);
                assert!(later.granted());
                assert_eq!(flash.clock.now_nanos(), base + ms(200));
                drop(later);
                assert_eq!(flash.active_count(), 0);
                drop(poll);
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn an_executing_poll_keeps_its_grant_when_an_old_driver_is_bridged() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let _runtime = runtime.enter();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let gate = flash.async_acquire(std::panic::Location::caller());
        assert!(gate.try_enter_poll());
        let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
        let notify_id = flash.next_condvar_id();
        let (receipt, advance) =
            flash.register_notify_async(notify_id, std::task::Waker::noop().clone());
        advance.fire();
        let receipt = receipt.expect("fresh notify has no stored permit");
        flash.signal_notify(notify_id);
        flash
            .core
            .lock()
            .registry
            .bridged
            .insert(credit::current_thread_key());
        let (later, advance) =
            flash.register_sleep_async(ms(100), std::task::Waker::noop().clone());
        advance.fire();
        pace_due(&flash, base);
        {
            let mut core = flash.core.lock();
            let advance = core.try_advance(&flash.clock);
            drop(core);
            advance.fire();
        }

        assert!(receipt.granted());
        assert!(!later.granted());
        assert_eq!(flash.clock.now_nanos(), base);
        assert_eq!(flash.active_count(), 1);
        drop(receipt);
        assert!(later.granted());
        drop(later);
        assert_eq!(flash.active_count(), 0);
        drop(poll);
    }

    #[kithara::test(native, flash(false))]
    fn a_completed_task_keeps_grant_credit_until_its_receipt_settles() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let _runtime = runtime.enter();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let gate = flash.async_acquire(std::panic::Location::caller());
        let diag = gate.diag();
        assert!(gate.try_enter_poll());
        let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
        let notify_id = flash.next_condvar_id();
        let (receipt, advance) =
            flash.register_notify_async(notify_id, std::task::Waker::noop().clone());
        advance.fire();
        let receipt = receipt.expect("fresh notify has no stored permit");
        drop(poll);
        assert!(matches!(
            flash.gate_park(&diag.state, gate.id()),
            super::super::state::ParkOutcome::Parked
        ));
        flash.signal_notify(notify_id);
        assert!(receipt.granted());
        assert!(flash.gate_drop(&diag.state, gate.id()).is_none());
        flash
            .core
            .lock()
            .registry
            .bridged
            .insert(credit::current_thread_key());
        let (later, advance) =
            flash.register_sleep_async(ms(100), std::task::Waker::noop().clone());
        advance.fire();
        pace_due(&flash, base);
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.core.lock().registry.active, 0);
        assert!(!later.granted());
        assert_eq!(flash.clock.now_nanos(), base);

        drop(receipt);
        assert!(later.granted());
        drop(later);
        assert_eq!(flash.active_count(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn a_late_grant_after_task_drop_is_retained_as_untracked_credit() {
        let flash = FlashInner::new_arc();
        let gate = flash.async_acquire(std::panic::Location::caller());
        let diag = gate.diag();
        assert!(gate.try_enter_poll());
        let poll = credit::AsyncPollGuard::enter(gate.id(), gate.loc());
        let notify_id = flash.next_condvar_id();
        let (receipt, advance) =
            flash.register_notify_async(notify_id, std::task::Waker::noop().clone());
        advance.fire();
        let receipt = receipt.expect("fresh notify has no stored permit");
        drop(poll);
        assert!(matches!(
            flash.gate_park(&diag.state, gate.id()),
            super::super::state::ParkOutcome::Parked
        ));
        assert!(flash.gate_drop(&diag.state, gate.id()).is_none());
        assert!(
            !flash
                .core
                .lock()
                .registry
                .task_diag
                .contains_key(&gate.id())
        );

        flash.signal_notify(notify_id);
        assert!(receipt.granted());
        assert_eq!(flash.active_count(), 1);
        assert_eq!(flash.core.lock().registry.active, 1);
        drop(receipt);
        assert_eq!(flash.active_count(), 0);
        assert_eq!(flash.core.lock().registry.active, 0);
    }

    #[kithara::test(native, flash(false))]
    fn pacer_fires_on_time_under_pacing() {
        let _guard = guard();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();

        flash.real_io_enter();
        let start = RealInstant::now();
        let waiter = spawn_park_for(&flash, Duration::from_millis(30));
        waiter.join().expect("waiter thread panicked");
        let elapsed = start.elapsed();
        flash.real_io_exit();

        assert_paced_elapsed(elapsed, 30);
        assert_eq!(flash.clock.now_nanos(), base + ms(30));
        assert_eq!(
            flash.advance_log(),
            vec![base + ms(30)],
            "paced advance sequence must stay deterministic"
        );
    }

    #[kithara::test(native, flash(false))]
    fn pacer_has_near_zero_wakes_for_far_deadline() {
        let _guard = guard();
        let flash = FlashInner::new_arc();

        flash.real_io_enter();
        let before = flash.pacer_wake_count();
        let waiter = spawn_park_for(&flash, Duration::from_millis(120));
        waiter.join().expect("waiter thread panicked");
        flash.real_io_exit();

        let wakes = flash.pacer_wake_count().saturating_sub(before);
        // A 1ms poll over a 120ms deadline would wake ~120 times; a self-unpark
        // busy-spin woke ~1e6. Event-driven wakes O(1): once at the deadline plus
        // the odd spurious `park_timeout` return under load. Bound well below the
        // poll count so a regression to either failure mode is still caught.
        assert!(
            wakes < 20,
            "event-driven pacer should wake O(1) times, not poll or busy-spin: {wakes}"
        );
    }

    /// Tests sharing one process share the engine, so an op in flight for one
    /// test can span a deadline another test registers much later. Real time
    /// banked before that deadline existed pays for it only up to the lag
    /// bound: a 30 s harness timeout fired a second after a 37 s neighbour
    /// began.
    #[kithara::test(native, flash(false), timeout(Short))]
    fn a_deadline_does_not_inherit_real_time_from_before_it_was_set() {
        let _guard = guard();
        let flash = FlashInner::new_arc();

        flash.real_io_enter();
        thread::sleep(Duration::from_millis(400));
        let start = RealInstant::now();
        let waiter = spawn_park_for(&flash, Duration::from_millis(200));
        waiter.join().expect("waiter thread panicked");
        let elapsed = start.elapsed();
        flash.real_io_exit();

        let lag = Duration::from_nanos(super::super::sched::consts::MAX_PACE_LAG_NANOS);
        assert_paced_elapsed(elapsed + lag, 200);
    }

    /// A timer that fires late leaves the clock trailing real time, and the
    /// next short deadline is paid from that lag rather than slept again;
    /// otherwise every short timer costs a whole OS sleep quantum.
    #[kithara::test(native, flash(false))]
    fn a_deadline_is_paid_from_the_lag_a_late_timer_left() {
        let _guard = guard();
        let flash = FlashInner::new_arc();

        flash.real_io_enter();
        thread::sleep(Duration::from_millis(45));
        let waiter = spawn_park_for(&flash, Duration::from_millis(40));
        let parked = waiter.join().expect("waiter thread panicked");
        flash.real_io_exit();

        assert!(
            parked < Duration::from_millis(40),
            "a deadline within the carried lag slept its full duration: {parked:?}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn pacer_retargets_earlier_deadline_mid_wait() {
        let _guard = guard();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();

        flash.real_io_enter();
        let start = RealInstant::now();
        let far = spawn_park_for(&flash, Duration::from_millis(350));
        wait_for_timed_count(&flash, 1);
        thread::sleep(Duration::from_millis(30));

        let near = spawn_park_for(&flash, Duration::from_millis(120));
        near.join().expect("near waiter thread panicked");
        let elapsed = start.elapsed();

        assert_paced_elapsed(elapsed, 120);
        assert!(
            elapsed < Duration::from_millis(250),
            "near deadline waited for the original far target: {elapsed:?}"
        );

        flash.real_io_exit();
        far.join().expect("far waiter thread panicked");
        assert_eq!(flash.advance_log(), vec![base + ms(120), base + ms(350)]);
    }

    #[kithara::test(native, flash(false))]
    fn pacer_wakes_on_quiescence_edge() {
        let _guard = guard();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let (release_tx, release_rx) = mpsc::channel();
        let (running_tx, running_rx) = mpsc::channel();

        // The pace anchors to real time inside `real_io_enter`, so the 80ms
        // deadline is owed 80ms of real time from the ANCHOR: real time spent
        // while the blocker still runs (thread spawns, channel handshakes) is
        // credit the pacer legally releases the instant quiescence begins.
        // Measure from the anchor — a post-handshake start is owed less than
        // the full target by exactly that credit, which host-scheduler
        // perturbation stretches past the assert's slack.
        flash.real_io_enter();
        let start = RealInstant::now();
        let blocker = {
            let flash = Arc::clone(&flash);
            thread::spawn(move || {
                bracketed_on(&flash, || {
                    running_tx.send(()).expect("send blocker running");
                    release_rx.recv().expect("receive blocker release");
                    flash.park_for(Duration::from_millis(300));
                });
            })
        };
        running_rx.recv().expect("receive blocker running");

        let waiter = spawn_park_for(&flash, Duration::from_millis(80));
        wait_for_timed_count(&flash, 1);
        assert_eq!(
            flash.clock.now_nanos(),
            base,
            "clock must hold still while a dedicated participant runs"
        );
        release_tx.send(()).expect("release blocker");

        waiter.join().expect("waiter thread panicked");
        let elapsed = start.elapsed();
        assert_paced_elapsed(elapsed, 80);

        flash.real_io_exit();
        blocker.join().expect("blocker thread panicked");
        assert_eq!(
            flash.advance_log(),
            vec![base + ms(80), base + ms(300)],
            "the quiescence edge advances the clock to the near deadline itself, \
             never to the blocker's later target"
        );
    }

    #[kithara::test(native, flash(false))]
    fn pacer_disarms_to_zero_and_rearms() {
        let _guard = guard();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();

        flash.real_io_enter();
        let first = spawn_park_for(&flash, Duration::from_millis(30));
        first.join().expect("first waiter thread panicked");
        flash.real_io_exit();
        assert_eq!(flash.clock.now_nanos(), base + ms(30));

        let collapse_start = RealInstant::now();
        let unpaced = spawn_park_for(&flash, Duration::from_secs(5));
        unpaced.join().expect("unpaced waiter thread panicked");
        assert!(
            collapse_start.elapsed() < Duration::from_secs(2),
            "deadline should collapse immediately after real_io exits"
        );
        let rearm_base = flash.clock.now_nanos();

        flash.real_io_enter();
        let start = RealInstant::now();
        let second = spawn_park_for(&flash, Duration::from_millis(30));
        second.join().expect("second waiter thread panicked");
        let elapsed = start.elapsed();
        flash.real_io_exit();

        assert_paced_elapsed(elapsed, 30);
        assert_eq!(flash.clock.now_nanos(), rearm_base + ms(30));
    }

    /// Every case above holds SYNC waiters only, so the pace promise was pinned
    /// just where no async slot exists. [`crate::flash::real_io`] promises pace,
    /// NOT pin: a deliberate virtual delay behind the op "still elapses at real
    /// pace, so the peer stays live". A task holding its slot across the op
    /// breaks it — `try_advance` vetoes on `pinning_async()` before it ever
    /// consults the pace limit, so the delay behind the op never elapses. That
    /// is the delayed-CDN hang: four stress tests, one dump signature
    /// (`active=0 active_async=2 real_io=5 pace_anchor=set`), the test server's
    /// own delay sitting unfired in `timed` while the tasks awaiting its
    /// response hold the very slots that freeze the clock it waits on. Those
    /// dumps name a `Running` holder — mid-poll — which the stranded-task rule
    /// cannot release, since that one requires `Runnable`.
    #[kithara::test(native, flash(false))]
    fn a_held_async_slot_does_not_veto_a_paced_deadline() {
        let _guard = guard();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();

        flash.real_io_enter();
        let slot = flash.test_hold_async();
        let start = RealInstant::now();
        let waiter = spawn_park_for(&flash, Duration::from_millis(30));
        wait_until(
            || flash.clock.now_nanos() >= base + ms(30),
            "the paced deadline behind the op to elapse",
        );
        let elapsed = start.elapsed();

        // Also guards the other direction: a fix that dropped pacing instead of
        // the veto would JUMP to the deadline, and the lower bound catches it.
        assert_paced_elapsed(elapsed, 30);

        drop(slot);
        flash.real_io_exit();
        waiter.join().expect("waiter thread panicked");
    }

    #[kithara::test(native, flash(false))]
    fn reset_preserves_pacer_wake() {
        let _guard = guard();
        let flash = FlashInner::new_arc();

        flash.real_io_enter();
        wait_until(
            || flash.pacer_wake_published(),
            "pacer wake handle publication",
        );
        flash.real_io_exit();
        flash.reset();

        let base = flash.clock.now_nanos();
        flash.real_io_enter();
        let start = RealInstant::now();
        let waiter = spawn_park_for(&flash, Duration::from_millis(30));
        waiter.join().expect("waiter thread panicked");
        let elapsed = start.elapsed();
        flash.real_io_exit();

        assert_paced_elapsed(elapsed, 30);
        assert_eq!(flash.clock.now_nanos(), base + ms(30));
    }
}
