//! Platform-optimal [`RwLock`], [`RwLockReadGuard`], and [`RwLockWriteGuard`].
//!
//! * **Native** — [`parking_lot::RwLock`].
//! * **WASM** — [`wasm_safe_thread::rwlock::RwLock`].

use std::ops::{Deref, DerefMut};

#[cfg(not(target_arch = "wasm32"))]
pub struct RwLock<T>(parking_lot::RwLock<T>);

#[cfg(not(target_arch = "wasm32"))]
impl<T> RwLock<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(parking_lot::RwLock::new(value))
    }

    #[inline]
    pub fn lock_sync_read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard(self.0.read())
    }

    #[inline]
    pub fn lock_sync_write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard(self.0.write())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct RwLockReadGuard<'a, T>(parking_lot::RwLockReadGuard<'a, T>);

#[cfg(not(target_arch = "wasm32"))]
impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct RwLockWriteGuard<'a, T>(parking_lot::RwLockWriteGuard<'a, T>);

#[cfg(not(target_arch = "wasm32"))]
impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

#[cfg(target_arch = "wasm32")]
pub struct RwLock<T>(wasm_safe_thread::rwlock::RwLock<T>);

#[cfg(target_arch = "wasm32")]
impl<T> RwLock<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(wasm_safe_thread::rwlock::RwLock::new(value))
    }

    #[inline]
    pub fn lock_sync_read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard(self.0.lock_sync_read())
    }

    #[inline]
    pub fn lock_sync_write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard(self.0.lock_sync_write())
    }
}

#[cfg(target_arch = "wasm32")]
impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

#[cfg(target_arch = "wasm32")]
pub struct RwLockReadGuard<'a, T>(wasm_safe_thread::guard::ReadGuard<'a, T>);

#[cfg(target_arch = "wasm32")]
impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(target_arch = "wasm32")]
pub struct RwLockWriteGuard<'a, T>(wasm_safe_thread::guard::WriteGuard<'a, T>);

#[cfg(target_arch = "wasm32")]
impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(target_arch = "wasm32")]
impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
