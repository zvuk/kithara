use core::ops::{Deref, DerefMut};

use ::loom::sync::{
    RwLock as LoomRwLock, RwLockReadGuard as LoomRwLockReadGuard,
    RwLockWriteGuard as LoomRwLockWriteGuard,
};

pub(crate) struct RwLock<T>(LoomRwLock<T>);

impl<T> RwLock<T> {
    #[inline]
    pub(crate) fn new(value: T) -> Self {
        Self(LoomRwLock::new(value))
    }

    #[inline]
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard(match self.0.read() {
            Ok(guard) => guard,
            Err(error) => error.into_inner(),
        })
    }

    #[inline]
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard(match self.0.write() {
            Ok(guard) => guard,
            Err(error) => error.into_inner(),
        })
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub(crate) struct RwLockReadGuard<'a, T>(LoomRwLockReadGuard<'a, T>);

pub(crate) struct RwLockWriteGuard<'a, T>(LoomRwLockWriteGuard<'a, T>);

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
