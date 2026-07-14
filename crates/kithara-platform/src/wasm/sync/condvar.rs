use web_time::Instant;

use super::mutex::MutexGuard;

type WstCondvar = wasm_safe_thread::condvar::Condvar;

pub struct Condvar(WstCondvar);

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
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        MutexGuard(self.0.wait_sync(guard.0))
    }

    #[inline]
    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        deadline: Instant,
    ) -> MutexGuard<'a, T> {
        let (g, _) = self.0.wait_sync_timeout(guard.0, deadline);
        MutexGuard(g)
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self(WstCondvar::new())
    }
}
