use core::ops::{Deref, DerefMut};

use parking_lot::Mutex as ParkingLotMutex;

use crate::common::error::NotAvailable;

pub(crate) struct Mutex<T>(ParkingLotMutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub(crate) fn new(value: T) -> Self {
        Self(ParkingLotMutex::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(MutexGuard($))]
            pub (crate) fn lock (& self) -> MutexGuard < '_ , T >;
            #[inline]
            #[expr($.map(MutexGuard).ok_or(NotAvailable))]
            pub (crate) fn try_lock (& self) -> Result < MutexGuard < '_ , T > , NotAvailable >;
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub(crate) struct MutexGuard<'a, T>(pub(crate) parking_lot::MutexGuard<'a, T>);

impl<T> MutexGuard<'_, T> {
    #[inline]
    pub(crate) fn unlocked<F: FnOnce()>(&mut self, f: F) {
        parking_lot::lock_api::MutexGuard::unlocked(&mut self.0, f);
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
