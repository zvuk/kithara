//! Platform-optimal [`Mutex`] and [`MutexGuard`].
//!
//! * **Native** — [`parking_lot::Mutex`] (no poisoning, smaller footprint).
//! * **WASM** — [`wasm_safe_thread::Mutex`] (safe on the main thread).

use std::ops::{Deref, DerefMut};

#[cfg(not(target_arch = "wasm32"))]
pub struct Mutex<T>(parking_lot::Mutex<T>);

#[cfg(not(target_arch = "wasm32"))]
impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(parking_lot::Mutex::new(value))
    }

    #[inline]
    pub fn lock_sync(&self) -> MutexGuard<'_, T> {
        MutexGuard(self.0.lock())
    }

    /// Try to acquire the lock without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the mutex is already held.
    #[inline]
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable> {
        self.0.try_lock().map(MutexGuard).ok_or(NotAvailable)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct MutexGuard<'a, T>(pub(super) parking_lot::MutexGuard<'a, T>);

#[cfg(not(target_arch = "wasm32"))]
impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

#[cfg(target_arch = "wasm32")]
pub struct Mutex<T>(wasm_safe_thread::Mutex<T>);

#[cfg(target_arch = "wasm32")]
impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(wasm_safe_thread::Mutex::new(value))
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

#[cfg(target_arch = "wasm32")]
impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[cfg(target_arch = "wasm32")]
pub struct MutexGuard<'a, T>(pub(super) wasm_safe_thread::guard::Guard<'a, T>);

#[cfg(target_arch = "wasm32")]
impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(target_arch = "wasm32")]
impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// `try_lock()` failed because the mutex is already held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAvailable;

impl std::fmt::Display for NotAvailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mutex is already locked")
    }
}

impl std::error::Error for NotAvailable {}
