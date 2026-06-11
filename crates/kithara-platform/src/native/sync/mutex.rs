use std::ops::{Deref, DerefMut};

use parking_lot::Mutex as ParkingLotMutex;

pub use crate::common::error::NotAvailable;

pub struct Mutex<T>(ParkingLotMutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(ParkingLotMutex::new(value))
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

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// crate-internal: condvar needs the raw guard
pub struct MutexGuard<'a, T>(pub(crate) parking_lot::MutexGuard<'a, T>);

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
