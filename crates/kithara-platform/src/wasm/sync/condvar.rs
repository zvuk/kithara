use web_time::Instant;

use super::mutex::MutexGuard;

type WstCondvar = wasm_safe_thread::condvar::Condvar;

pub struct Condvar(WstCondvar);

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

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
