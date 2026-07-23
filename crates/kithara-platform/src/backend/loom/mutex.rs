use core::ops::{Deref, DerefMut};

use crate::{common::error::NotAvailable, loom::sync as backend};

pub struct Mutex<T>(backend::Mutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(backend::Mutex::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(MutexGuard($))]
            pub fn lock(&self) -> MutexGuard<'_, T>;
            #[inline]
            #[expr($.map(MutexGuard))]
            pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable>;
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub struct MutexGuard<'a, T>(pub(crate) backend::MutexGuard<'a, T>);

impl<T> MutexGuard<'_, T> {
    #[inline]
    pub fn unlocked<F: FnOnce()>(&mut self, f: F) {
        self.0.unlocked(f);
    }
}

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
