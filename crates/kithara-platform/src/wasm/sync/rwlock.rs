use std::ops::{Deref, DerefMut};

type WstRwLock<T> = wasm_safe_thread::rwlock::RwLock<T>;

pub struct RwLock<T>(WstRwLock<T>);

impl<T> RwLock<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(WstRwLock::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(RwLockReadGuard($))]
            #[call(lock_sync_read)]
            pub fn read (& self) -> RwLockReadGuard < '_ , T >;
            #[inline]
            #[expr(RwLockWriteGuard($))]
            #[call(lock_sync_write)]
            pub fn write (& self) -> RwLockWriteGuard < '_ , T >;
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub struct RwLockReadGuard<'a, T>(wasm_safe_thread::guard::ReadGuard<'a, T>);

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

pub struct RwLockWriteGuard<'a, T>(wasm_safe_thread::guard::WriteGuard<'a, T>);

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
