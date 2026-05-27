#[cfg(not(target_arch = "wasm32"))]
use parking_lot::Condvar as ParkingLotCondvar;

use super::MutexGuard;

#[cfg(not(target_arch = "wasm32"))]
pub struct Condvar(ParkingLotCondvar);

#[cfg(not(target_arch = "wasm32"))]
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
        deadline: web_time::Instant,
    ) -> MutexGuard<'a, T> {
        let _ = self.0.wait_until(&mut guard.0, deadline);
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
        deadline: web_time::Instant,
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
