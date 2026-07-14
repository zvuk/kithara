use super::MutexGuard;
use crate::{common::time::Instant, loom::sync as backend};

#[derive(Default)]
pub struct Condvar(backend::Condvar);

impl Condvar {
    delegate::delegate! {
        to self.0 {
            #[inline]
            pub fn notify_all(&self);
            #[inline]
            pub fn notify_one(&self);
        }
    }
    #[inline]
    #[must_use]
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        MutexGuard(self.0.wait(guard.0))
    }

    #[inline]
    #[must_use]
    pub fn wait_timeout<'a, T>(
        &self,
        mut guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        self.0.wait_timeout_ref(&mut guard.0, deadline);
        guard
    }
}
