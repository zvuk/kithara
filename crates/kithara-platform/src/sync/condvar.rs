#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
use parking_lot::Condvar as ParkingLotCondvar;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
use parking_lot::lock_api::MutexGuard as RawMutexGuard;

use super::MutexGuard;
use crate::time::Instant;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
use crate::time::sim::sched;

/// Native condvar backed by `parking_lot`.
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
pub struct Condvar(ParkingLotCondvar);

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
impl Condvar {
    #[inline]
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self(ParkingLotCondvar::new())
    }

    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }

    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    #[inline]
    pub fn wait_sync<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.0.wait(&mut guard.0);
        guard
    }

    #[inline]
    pub fn wait_sync_timeout<'a, T>(
        &self,
        mut guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        // `deadline` is the real `std::time::Instant` (via `web_time`).
        let _ = self.0.wait_until(&mut guard.0, deadline);
        guard
    }
}

/// Native condvar under `flash-time`: the real `parking_lot::Condvar` is unused.
/// Waits register a waiter on the quiescence engine (keyed by `cvid`) and
/// `notify_*` signal that group, so a timed wait collapses the virtual clock
/// instead of consuming real wall-clock.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub struct Condvar {
    cvid: u64,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
impl Condvar {
    #[inline]
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self {
            cvid: sched::next_condvar_id(),
        }
    }

    #[inline]
    pub fn notify_all(&self) {
        sched::signal_condvar(self.cvid, true);
    }

    #[inline]
    pub fn notify_one(&self) {
        sched::signal_condvar(self.cvid, false);
    }

    /// Untimed wait: register an untimed waiter (no deadline) BEFORE releasing
    /// the domain guard, so any predicate change + notify (which must take the
    /// domain lock, then signal) is serialized after our engine entry. Block
    /// off-lock with the guard released, re-acquire on wake.
    #[inline]
    #[must_use]
    pub fn wait_sync<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        let (token, adv) = sched::register_condvar_untimed(self.cvid);
        RawMutexGuard::unlocked(&mut guard.0, move || {
            sched::fire_advance(adv);
            token.wait();
            sched::mark_running_after_condvar();
        });
        guard
    }

    /// Timed wait: register a timed engine waiter (deadline = `deadline`'s
    /// absolute virtual nanos) BEFORE releasing the domain guard, fire any
    /// advance-due tokens, then block off-lock until the engine crosses the
    /// deadline OR a `notify_*` signals our `cvid`. Re-acquire on wake; the
    /// caller re-checks its predicate (storage loops). The engine entry is
    /// registered under the domain guard so a concurrent predicate-change +
    /// notify cannot land between the caller's predicate check and our park.
    #[inline]
    #[must_use]
    pub fn wait_sync_timeout<'a, T>(
        &self,
        mut guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        let (token, adv) = sched::register_condvar_timed(deadline.as_virtual_nanos(), self.cvid);
        RawMutexGuard::unlocked(&mut guard.0, move || {
            sched::fire_advance(adv);
            token.wait();
            sched::mark_running_after_condvar();
        });
        guard
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "wasm32")]
type WstCondvar = wasm_safe_thread::condvar::Condvar;

#[cfg(target_arch = "wasm32")]
pub struct Condvar(WstCondvar);

#[cfg(target_arch = "wasm32")]
impl Condvar {
    #[inline]
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self(WstCondvar::new())
    }

    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }

    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    #[inline]
    pub fn wait_sync<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        MutexGuard(self.0.wait_sync(guard.0))
    }

    #[inline]
    pub fn wait_sync_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        let (g, _) = self.0.wait_sync_timeout(guard.0, deadline);
        MutexGuard(g)
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
