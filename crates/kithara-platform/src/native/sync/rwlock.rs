use std::ops::{Deref, DerefMut};

use parking_lot::RwLock as ParkingLotRwLock;

pub struct RwLock<T>(ParkingLotRwLock<T>);

impl<T> RwLock<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(ParkingLotRwLock::new(value))
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

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub struct RwLockReadGuard<'a, T>(parking_lot::RwLockReadGuard<'a, T>);

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

pub struct RwLockWriteGuard<'a, T>(parking_lot::RwLockWriteGuard<'a, T>);

impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
