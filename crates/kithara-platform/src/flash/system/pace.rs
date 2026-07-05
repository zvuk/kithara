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
    pub(in crate::flash) fn real_io_enter(&self) {
        self.pacer.spawn.call_once(|| {
            let owner = self
                .pacer
                .owner
                .upgrade()
                .expect("BUG: real_io_enter is reachable only through a live Arc<FlashInner>");
            // Raw std thread: the pacer must stay INVISIBLE to the engine (a
            // platform `spawn_named` would count it as a dedicated pacer and pin
            // the very clock it exists to advance).
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
        sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
        thread,
        time::Instant as RealInstant,
    };

    use super::*;
    use crate::flash::{Duration, system::credit};

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
        credit::mark_dedicated();
        body();
        flash.on_participant_exit();
    }

    fn spawn_park_for(flash: &Arc<FlashInner>, duration: Duration) -> thread::JoinHandle<()> {
        let flash = Arc::clone(flash);
        thread::spawn(move || bracketed_on(&flash, || flash.park_for(duration)))
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

    #[test]
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

    #[test]
    fn pacer_has_near_zero_wakes_for_far_deadline() {
        let _guard = guard();
        let flash = FlashInner::new_arc();

        flash.real_io_enter();
        let before = flash.pacer_wake_count();
        let waiter = spawn_park_for(&flash, Duration::from_millis(120));
        waiter.join().expect("waiter thread panicked");
        flash.real_io_exit();

        let wakes = flash.pacer_wake_count().saturating_sub(before);
        assert!(
            wakes <= 5,
            "event-driven pacer should wake O(1) times, not poll every millisecond: {wakes}"
        );
    }

    #[test]
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
        assert_eq!(flash.clock.now_nanos(), base + ms(120));

        flash.real_io_exit();
        far.join().expect("far waiter thread panicked");
        assert_eq!(flash.advance_log(), vec![base + ms(120), base + ms(350)]);
    }

    #[test]
    fn pacer_wakes_on_quiescence_edge() {
        let _guard = guard();
        let flash = FlashInner::new_arc();
        let base = flash.clock.now_nanos();
        let (release_tx, release_rx) = mpsc::channel();
        let (running_tx, running_rx) = mpsc::channel();

        flash.real_io_enter();
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

        let start = RealInstant::now();
        let waiter = spawn_park_for(&flash, Duration::from_millis(80));
        wait_for_timed_count(&flash, 1);
        release_tx.send(()).expect("release blocker");

        waiter.join().expect("waiter thread panicked");
        let elapsed = start.elapsed();
        assert_paced_elapsed(elapsed, 80);
        assert_eq!(flash.clock.now_nanos(), base + ms(80));

        flash.real_io_exit();
        blocker.join().expect("blocker thread panicked");
    }

    #[test]
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

    #[test]
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
