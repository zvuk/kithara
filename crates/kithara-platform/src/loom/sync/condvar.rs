use ::loom::sync::Condvar as LoomCondvar;

use super::mutex::MutexGuard;
use crate::common::time::Instant;

#[derive(Default)]
pub(crate) struct Condvar(LoomCondvar);

impl Condvar {
    #[inline]
    pub(crate) fn notify_all(&self) {
        self.0.notify_all();
    }

    #[inline]
    pub(crate) fn notify_one(&self) {
        self.0.notify_one();
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
        let Some(inner) = guard.inner.take() else {
            unreachable!("condvar received an unlocked mutex guard");
        };
        guard.inner = Some(match self.0.wait(inner) {
            Ok(inner) => inner,
            Err(error) => error.into_inner(),
        });
    }

    #[track_caller]
    pub(crate) fn wait_timeout_ref<T>(&self, _guard: &mut MutexGuard<'_, T>, _deadline: Instant) {
        panic!("timed condvar waits require the flash backend when loom is enabled");
    }
}
