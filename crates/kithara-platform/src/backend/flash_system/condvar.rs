use parking_lot::Condvar as ParkingLotCondvar;

use super::mutex::MutexGuard;
use crate::common::time::Instant;

#[derive(Default)]
pub(crate) struct Condvar(ParkingLotCondvar);

impl Condvar {
    delegate::delegate! {
        to self.0 {
            #[inline]
            pub(crate) fn notify_all(&self);
            #[inline]
            pub(crate) fn notify_one(&self);
        }
    }

    #[inline]
    #[track_caller]
    pub(crate) fn wait<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait_ref(&mut guard);
        guard
    }

    #[inline]
    #[track_caller]
    pub(crate) fn wait_ref<T>(&self, guard: &mut MutexGuard<'_, T>) {
        crate::no_block::forbid("Condvar::wait");
        self.0.wait(&mut guard.0);
    }

    #[inline]
    #[track_caller]
    pub(crate) fn wait_timeout_ref<T>(&self, guard: &mut MutexGuard<'_, T>, deadline: Instant) {
        crate::no_block::forbid("Condvar::wait_timeout");
        let _ = self.0.wait_until(&mut guard.0, deadline);
    }
}
