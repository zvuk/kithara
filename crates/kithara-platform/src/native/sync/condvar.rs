use parking_lot::Condvar as ParkingLotCondvar;

use super::mutex::MutexGuard;
use crate::common::time::Instant;

/// Native condvar backed by `parking_lot`.
#[derive(Default)]
pub struct Condvar(ParkingLotCondvar);

impl Condvar {
    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }

    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    #[inline]
    #[track_caller]
    pub fn wait<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait_ref(&mut guard);
        guard
    }

    #[inline]
    #[track_caller]
    pub fn wait_timeout<'a, T>(
        &self,
        mut guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        self.wait_timeout_ref(&mut guard, deadline);
        guard
    }

    /// In-place [`wait`](Self::wait): atomically release the guard's mutex and
    /// park, re-acquiring on wake — without consuming the guard. The by-value
    /// `wait` delegates here (so this is exercised in production), and the flash
    /// facade calls it directly to keep its own holder bookkeeping across the
    /// wait without moving the guard (its `Drop` forbids the move).
    #[inline]
    #[track_caller]
    pub(crate) fn wait_ref<T>(&self, guard: &mut MutexGuard<'_, T>) {
        crate::no_block::forbid("Condvar::wait");
        self.0.wait(&mut guard.0);
    }

    /// In-place [`wait_timeout`](Self::wait_timeout). `deadline` is the real
    /// `std::time::Instant` (via `web_time`).
    #[inline]
    #[track_caller]
    pub(crate) fn wait_timeout_ref<T>(&self, guard: &mut MutexGuard<'_, T>, deadline: Instant) {
        crate::no_block::forbid("Condvar::wait_timeout");
        let _ = self.0.wait_until(&mut guard.0, deadline);
    }
}
