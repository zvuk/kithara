//! Platform-optimal [`Condvar`] and [`WaitTimeoutResult`].
//!
//! * **Native** — [`parking_lot::Condvar`].
//! * **WASM** — [`wasm_safe_thread::condvar::Condvar`].

use super::MutexGuard;

// ── Native ──────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub struct Condvar(parking_lot::Condvar);

#[cfg(not(target_arch = "wasm32"))]
impl Condvar {
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self(parking_lot::Condvar::new())
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
    ) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
        let result = self.0.wait_until(&mut guard.0, deadline);
        (guard, WaitTimeoutResult(result.timed_out()))
    }

    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

// ── WASM ────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
pub struct Condvar(wasm_safe_thread::condvar::Condvar);

#[cfg(target_arch = "wasm32")]
impl Condvar {
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self(wasm_safe_thread::condvar::Condvar::new())
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
    ) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
        let (g, result) = self.0.wait_sync_timeout(guard.0, deadline);
        (MutexGuard(g), WaitTimeoutResult(result.timed_out()))
    }

    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

// ── Shared ──────────────────────────────────────────────────────────

/// Result of a timed wait on a [`Condvar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns `true` if the wait timed out (deadline elapsed).
    #[inline]
    #[must_use]
    pub fn timed_out(&self) -> bool {
        self.0
    }
}
