use std::ops::{Deref, DerefMut};

use parking_lot::Mutex as ParkingLotMutex;

pub use crate::common::error::NotAvailable;

pub struct Mutex<T>(ParkingLotMutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(ParkingLotMutex::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(MutexGuard($))]
            pub fn lock (& self) -> MutexGuard < '_ , T >;
            /// Try to acquire the lock without blocking.
            ///
            /// # Errors
            ///
            /// Returns [`NotAvailable`] if the mutex is already held.
            #[inline]
            #[expr($.map(MutexGuard).ok_or(NotAvailable))]
            pub fn try_lock (& self) -> Result < MutexGuard < '_ , T > , NotAvailable >;
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// crate-internal: condvar needs the raw guard
pub struct MutexGuard<'a, T>(pub(crate) parking_lot::MutexGuard<'a, T>);

impl<T> MutexGuard<'_, T> {
    /// Temporarily unlock the mutex, run `f`, then relock before returning.
    ///
    /// The data may change while unlocked: re-validate any predicate after
    /// the call (classic condvar-style usage).
    #[inline]
    pub fn unlocked<F: FnOnce()>(&mut self, f: F) {
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

#[cfg(test)]
mod tests {
    use super::Mutex;

    #[test]
    fn unlocked_releases_and_relocks() {
        let m = Mutex::new(1);
        let mut g = m.lock();
        g.unlocked(|| {
            // The mutex is released inside `f`: an independent lock succeeds.
            *m.lock() += 1;
        });
        // Relocked: while the guard is alive again the mutex must be held.
        assert!(m.try_lock().is_err());
        // And the guard observes the mutation made while unlocked.
        assert_eq!(*g, 2);
        drop(g);
        assert!(m.try_lock().is_ok());
    }
}
