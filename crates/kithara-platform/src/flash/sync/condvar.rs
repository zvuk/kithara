use parking_lot::{Condvar as ParkingLotCondvar, lock_api::MutexGuard as RawMutexGuard};

use super::mutex::MutexGuard;
use crate::flash::{
    Instant, flash_ambient,
    system::{self, credit},
};

/// Native condvar under `flash`. Each operation branches on
/// [`flash_ambient`]: when the test is flash-eligible, waits register on the
/// quiescence engine (keyed by `cvid`) and `notify_*` signal that group, so a
/// timed wait collapses the virtual clock; otherwise the real
/// `parking_lot::Condvar` drives a true wall-clock wait/wake (the default-real
/// path, and the only path until `#[kithara::flash]` annotations land). The
/// engine `cvid` and the real condvar share the SAME domain mutex, so the
/// unified predicate state is consistent across both park/wake mechanisms.
pub struct Condvar {
    cvid: u64,
    real: ParkingLotCondvar,
    /// Mechanism captured ONCE at construction: `true` ⇒ engine-backed (the
    /// creating context was flash-eligible), `false` ⇒ real `parking_lot`. Fixed
    /// for the condvar's life so EVERY caller — wait AND notify, on ANY thread —
    /// uses the SAME mechanism. A notifier on a thread that did not inherit the
    /// test's ambient (e.g. a raw `std::thread::spawn`, which does not propagate
    /// it) still reaches an engine-parked waiter. Selecting per-call on
    /// `flash_ambient()` instead would diverge across threads of different
    /// ambient and silently lose the wakeup (the waiter parks on the engine while
    /// the notifier signals the real condvar, or vice versa).
    engine: bool,
}

impl Condvar {
    #[inline]
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self {
            cvid: system::next_condvar_id(),
            real: ParkingLotCondvar::new(),
            engine: flash_ambient(),
        }
    }

    #[inline]
    pub fn notify_all(&self) {
        if self.engine {
            system::signal_condvar(self.cvid, true);
        } else {
            self.real.notify_all();
        }
    }

    #[inline]
    pub fn notify_one(&self) {
        if self.engine {
            system::signal_condvar(self.cvid, false);
        } else {
            self.real.notify_one();
        }
    }

    /// Untimed wait. Flash: register an untimed engine waiter (no deadline)
    /// BEFORE releasing the domain guard, so any predicate change + notify
    /// (which must take the domain lock, then signal) is serialized after our
    /// engine entry; block off-lock with the guard released, re-acquire on wake.
    /// Real: `parking_lot::Condvar::wait` atomically releases the domain guard
    /// and parks, re-acquiring on wake — the same lost-wakeup-free handshake on
    /// the real lock.
    #[inline]
    #[must_use]
    pub fn wait_sync<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        if self.engine {
            let (token, adv) = system::register_condvar_untimed(self.cvid);
            RawMutexGuard::unlocked(&mut guard.0, move || {
                system::fire_advance(adv);
                token.wait();
                credit::mark_running_after_condvar();
            });
        } else {
            self.real.wait(&mut guard.0);
        }
        guard
    }

    /// Timed wait. Flash: register a timed engine waiter (deadline = `deadline`'s
    /// absolute virtual nanos) BEFORE releasing the domain guard, fire any
    /// advance-due tokens, then block off-lock until the engine crosses the
    /// deadline OR a `notify_*` signals our `cvid`. Real: `wait_for` the
    /// remaining wall-clock budget (`deadline` is virtual-`Instant` over the
    /// real monotonic clock when flash is off), so the real condvar wakes on a
    /// `notify_*` or the timeout. Re-acquire on wake; the caller re-checks its
    /// predicate (storage loops). The engine/real entry is taken under the
    /// domain guard so a concurrent predicate-change + notify cannot land
    /// between the caller's predicate check and our park.
    #[inline]
    #[must_use]
    pub fn wait_sync_timeout<'a, T>(
        &self,
        mut guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        if self.engine {
            let (token, adv) =
                system::register_condvar_timed(deadline.as_virtual_nanos(), self.cvid);
            RawMutexGuard::unlocked(&mut guard.0, move || {
                system::fire_advance(adv);
                token.wait();
                credit::mark_running_after_condvar();
            });
        } else {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = self.real.wait_for(&mut guard.0, remaining);
        }
        guard
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
