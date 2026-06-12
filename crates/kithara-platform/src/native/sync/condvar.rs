use parking_lot::Condvar as ParkingLotCondvar;

use super::mutex::MutexGuard;
use crate::common::time::Instant;

/// Native condvar backed by `parking_lot`.
pub struct Condvar(ParkingLotCondvar);

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

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
