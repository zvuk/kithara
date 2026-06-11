//! Real-I/O pacing: while a real socket operation is in flight, the virtual
//! clock may not outrun real time. The engine cannot see real-world transit
//! (a parked socket await releases its quiescence slot), so without pacing it
//! jumps to the next virtual deadline — firing watchdogs/timeouts spuriously
//! ahead of bytes that are still on the wire. Pacing (not pinning) keeps a
//! virtually-delayed peer live: its deadline still fires once the equivalent
//! REAL time has passed. See the crate README ("Real I/O pacing") and
//! [`super::RealIoScope`].

use std::sync::{Once, OnceLock, atomic::Ordering};

use parking_lot::{Condvar, Mutex};
use web_time::Instant as RealInstant;

use super::{
    Duration, SIM_NANOS,
    sched::{self, SCHED},
};

/// Pacer arming flag + wakeup. `armed` is locked BEFORE `SCHED` (strict
/// `armed -> SCHED` order shared by [`real_io_enter`], [`real_io_exit`] and
/// the pacer loop), so a disarm racing a fresh enter can never leave the
/// pacer asleep while `real_io > 0`.
struct Pacer {
    armed: Mutex<bool>,
    cv: Condvar,
}

impl Pacer {
    /// Real-time tick between deferred-advance retries while real I/O is in
    /// flight: a paced deadline fires within one tick of the equivalent real
    /// time having passed.
    const TICK: Duration = Duration::from_millis(1);

    fn run(&self) {
        loop {
            {
                let mut armed = self.armed.lock();
                while !*armed {
                    self.cv.wait(&mut armed);
                }
            }
            // REAL sleep: the pacer exists precisely to feed real time into
            // the engine while real I/O is in flight.
            std::thread::sleep(Self::TICK);
            let mut s = SCHED.lock();
            let adv = sched::try_advance_locked(&mut s);
            drop(s);
            sched::fire_advance(adv);
        }
    }
}

static PACER: OnceLock<Pacer> = OnceLock::new();

fn handle() -> &'static Pacer {
    static SPAWN: Once = Once::new();
    let p = PACER.get_or_init(|| Pacer {
        armed: Mutex::new(false),
        cv: Condvar::new(),
    });
    SPAWN.call_once(|| {
        // Raw std thread: the pacer must stay INVISIBLE to the engine (a
        // platform `spawn_named` would count it as a dedicated pacer and pin
        // the very clock it exists to advance).
        std::thread::Builder::new()
            .name("kithara-flash-io-pacer".into())
            .spawn(|| handle().run())
            .expect("BUG: spawning the flash io-pacer thread cannot fail");
    });
    p
}

/// Mark ONE real I/O operation in flight. The first op anchors the pace to
/// the current (real, virtual) instant and arms the pacer thread.
pub(super) fn real_io_enter() {
    let h = handle();
    let mut armed = h.armed.lock();
    let mut s = SCHED.lock();
    s.real_io += 1;
    if s.real_io == 1 {
        s.pace_anchor = Some((RealInstant::now(), SIM_NANOS.load(Ordering::Acquire)));
    }
    drop(s);
    if !*armed {
        // Set under the lock, notify after release: the pacer re-checks
        // `*armed` under the same lock, so the wakeup cannot be lost.
        *armed = true;
        drop(armed);
        h.cv.notify_one();
    }
}

/// Complete ONE real I/O operation. The last completion clears the anchor,
/// disarms the pacer and immediately re-runs the advance rule: full-speed
/// collapse resumes.
pub(super) fn real_io_exit() {
    let h = handle();
    let mut armed = h.armed.lock();
    let mut s = SCHED.lock();
    debug_assert!(s.real_io > 0, "real_io exit without a matching enter");
    s.real_io = s.real_io.saturating_sub(1);
    if s.real_io != 0 {
        return;
    }
    s.pace_anchor = None;
    *armed = false;
    let adv = sched::try_advance_locked(&mut s);
    drop(s);
    drop(armed);
    sched::fire_advance(adv);
}
