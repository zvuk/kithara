use core::ops::{Deref, DerefMut};

use parking_lot::Mutex as ParkingLotMutex;

use crate::common::error::NotAvailable;

pub(crate) struct Mutex<T>(ParkingLotMutex<T>);

impl<T> Mutex<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(ParkingLotMutex::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[expr(MutexGuard($))]
            pub(crate) fn lock(&self) -> MutexGuard<'_, T>;
            #[expr($.map(MutexGuard).ok_or(NotAvailable))]
            pub(crate) fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable>;
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub(crate) struct MutexGuard<'a, T>(parking_lot::MutexGuard<'a, T>);

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
