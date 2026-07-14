use core::ops::{Deref, DerefMut};

use crate::loom::sync::rwlock as backend;

pub struct RwLock<T>(backend::RwLock<T>);

impl<T> RwLock<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(backend::RwLock::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(RwLockReadGuard($))]
            pub fn read(&self) -> RwLockReadGuard<'_, T>;
            #[inline]
            #[expr(RwLockWriteGuard($))]
            pub fn write(&self) -> RwLockWriteGuard<'_, T>;
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub struct RwLockReadGuard<'a, T>(backend::RwLockReadGuard<'a, T>);

pub struct RwLockWriteGuard<'a, T>(backend::RwLockWriteGuard<'a, T>);

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
