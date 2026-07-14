use core::ops::{Deref, DerefMut};

use parking_lot::RwLock as ParkingLotRwLock;

pub(crate) struct RwLock<T>(ParkingLotRwLock<T>);

impl<T> RwLock<T> {
    #[inline]
    pub(crate) fn new(value: T) -> Self {
        Self(ParkingLotRwLock::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(RwLockReadGuard($))]
            pub(crate) fn read(&self) -> RwLockReadGuard<'_, T>;
            #[inline]
            #[expr(RwLockWriteGuard($))]
            pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T>;
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub(crate) struct RwLockReadGuard<'a, T>(parking_lot::RwLockReadGuard<'a, T>);

pub(crate) struct RwLockWriteGuard<'a, T>(parking_lot::RwLockWriteGuard<'a, T>);

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

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
