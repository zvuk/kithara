//! Real-I/O pacing: while a real socket operation is in flight, the virtual
//! clock may not outrun real time. The engine cannot see real-world transit
//! (a parked socket await releases its quiescence slot), so without pacing it
//! jumps to the next virtual deadline — firing watchdogs/timeouts spuriously
//! ahead of bytes that are still on the wire. Pacing (not pinning) keeps a
//! virtually-delayed peer live: its deadline still fires once the equivalent
//! REAL time has passed. See the crate README ("Real I/O pacing") and
//! [`crate::flash::RealIoScope`].

use std::sync::{Once, Weak};

use super::{FLASH, FlashInner};
use crate::{
    common::time::Instant as RealInstant,
    flash::Duration,
    native::sync::{Condvar, Mutex},
};

/// Per-instance pacer arming flag + wakeup + the lazily-spawned pacer thread.
/// `armed` is locked BEFORE `core` (strict `armed -> core` order shared by
/// [`FlashInner::real_io_enter`], [`FlashInner::real_io_exit`] and the pacer
/// loop), so a disarm racing a fresh enter can never leave the pacer asleep
/// while `real_io > 0`.
pub(super) struct Pacer {
    armed: Mutex<bool>,
    cv: Condvar,
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
            armed: Mutex::new(false),
            cv: Condvar::new(),
            spawn: Once::new(),
            owner,
        }
    }
}

impl FlashInner {
    /// Eternal pacer loop, run on the raw pacer thread holding a strong `Arc`
    /// to this instance. The wait-while-`!armed` park is UNTIMED on purpose: a
    /// disarmed pacer consumes zero CPU until the next arm.
    fn pace_run(&self) {
        loop {
            {
                let mut armed = self.pacer.armed.lock_sync();
                while !*armed {
                    armed = self.pacer.cv.wait_sync(armed);
                }
            }
            // REAL sleep: the pacer exists precisely to feed real time into
            // the engine while real I/O is in flight.
            crate::native::thread::sleep(Pacer::TICK);
            let mut s = self.core.lock_sync();
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
        let mut armed = self.pacer.armed.lock_sync();
        let mut s = self.core.lock_sync();
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
        let mut armed = self.pacer.armed.lock_sync();
        let mut s = self.core.lock_sync();
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

/// Process-engine forward of [`FlashInner::real_io_enter`].
pub(in crate::flash) fn real_io_enter() {
    FLASH.real_io_enter();
}

/// Process-engine forward of [`FlashInner::real_io_exit`].
pub(in crate::flash) fn real_io_exit() {
    FLASH.real_io_exit();
}
