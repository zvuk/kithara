use std::sync::{Once, Weak};

use super::{FLASH, FlashInner};
use crate::{
    common::time::Instant as RealInstant,
    flash::Duration,
    native::sync::{Condvar, Mutex, MutexGuard},
};

/// Per-instance pacer arming flag + wakeup + the lazily-spawned pacer thread.
/// `armed` is locked BEFORE `core` (strict `armed -> core` order shared by
/// [`FlashInner::real_io_enter`], [`FlashInner::real_io_exit`] and the pacer
/// loop), so a disarm racing a fresh enter can never leave the pacer asleep
/// while `real_io > 0`.
pub(super) struct Pacer {
    cv: Condvar,
    armed: Mutex<bool>,
    /// One-shot lazy spawn of the pacer thread, on the first arm
    /// ([`FlashInner::real_io_enter`]).
    spawn: Once,
    /// Weak self-reference installed by `Arc::new_cyclic` at construction; the
    /// spawn site upgrades it ONCE to hand the eternal pacer thread a strong
    /// `Arc` to its OWN instance (a `&self` method cannot mint an `Arc`).
    owner: Weak<FlashInner>,
}

impl Pacer {
    /// Real-time tick between deferred-advance retries while real I/O is in
    /// flight: a paced deadline fires within one tick of the equivalent real
    /// time having passed.
    const TICK: Duration = Duration::from_millis(1);

    pub(super) fn new(owner: Weak<FlashInner>) -> Self {
        Self {
            owner,
            armed: Mutex::default(),
            cv: Condvar::default(),
            spawn: Once::new(),
        }
    }
}

impl FlashInner {
    /// Eternal pacer loop, run on the raw pacer thread holding a strong `Arc`
    /// to this instance. The disarmed park ([`wait_armed`]) is UNTIMED on
    /// purpose: a disarmed pacer consumes zero CPU until the next arm.
    fn pace_run(&self) {
        loop {
            wait_armed(&self.pacer.cv, self.pacer.armed.lock());
            // REAL sleep: the pacer exists precisely to feed real time into
            // the engine while real I/O is in flight.
            crate::native::thread::sleep(Pacer::TICK);
            let mut s = self.core.lock();
            let adv = s.try_advance(&self.clock);
            drop(s);
            adv.fire();
        }
    }

    /// Mark ONE real I/O operation in flight. The first op anchors the pace to
    /// the current (real, virtual) instant and arms the pacer thread (spawned
    /// lazily on the first arm).
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
        let mut armed = self.pacer.armed.lock();
        let mut s = self.core.lock();
        s.sched.real_io += 1;
        if s.sched.real_io == 1 {
            s.sched.pace_anchor = Some((RealInstant::now(), self.clock.now_nanos()));
        }
        drop(s);
        if !*armed {
            // Set under the lock, notify after release: the pacer re-checks
            // `*armed` under the same lock, so the wakeup cannot be lost.
            *armed = true;
            drop(armed);
            self.pacer.cv.notify_one();
        }
    }

    /// Complete ONE real I/O operation. The last completion clears the anchor,
    /// disarms the pacer and immediately re-runs the advance rule: full-speed
    /// collapse resumes.
    pub(in crate::flash) fn real_io_exit(&self) {
        let mut armed = self.pacer.armed.lock();
        let mut s = self.core.lock();
        debug_assert!(s.sched.real_io > 0, "real_io exit without a matching enter");
        s.sched.real_io = s.sched.real_io.saturating_sub(1);
        if s.sched.real_io != 0 {
            return;
        }
        s.sched.pace_anchor = None;
        *armed = false;
        let adv = s.try_advance(&self.clock);
        drop(s);
        drop(armed);
        adv.fire();
    }
}

/// The disarmed park of [`FlashInner::pace_run`]: `armed` is checked under the
/// lock before every block and re-checked after every wake; UNTIMED on purpose
/// (a disarmed pacer consumes zero CPU until the next arm). The guard enters
/// as a parameter and drops on return — released before the caller's real-time
/// tick, preserving the strict `armed -> core` lock order (parameter form
/// keeps clippy's `significant_drop_tightening` from mis-suggesting an early
/// drop inside the loop).
fn wait_armed(cv: &Condvar, mut armed: MutexGuard<'_, bool>) {
    while !*armed {
        armed = cv.wait(armed);
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
