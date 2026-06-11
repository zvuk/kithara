use std::ops::{Deref, DerefMut};

use wasm_safe_thread::Mutex as WasmMutex;

pub use crate::common::error::NotAvailable;

pub struct Mutex<T>(WasmMutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(WasmMutex::new(value))
    }

    #[inline]
    pub fn lock_sync(&self) -> MutexGuard<'_, T> {
        MutexGuard(self.0.lock_sync())
    }

    /// Try to acquire the lock without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the mutex is already held.
    #[inline]
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable> {
        self.0.try_lock().map(MutexGuard).map_err(|_| NotAvailable)
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// SAFETY: browser is single-threaded; a `Mutex` value never crosses threads.
unsafe impl<T> Send for Mutex<T> {}
// SAFETY: same — no concurrent access on the single browser thread.
unsafe impl<T> Sync for Mutex<T> {}

// crate-internal: condvar needs the raw guard
pub struct MutexGuard<'a, T>(pub(crate) wasm_safe_thread::guard::Guard<'a, T>);

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
