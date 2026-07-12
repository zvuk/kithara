use core::ops::{Deref, DerefMut};

use ::loom::sync::{Mutex as LoomMutex, MutexGuard as LoomMutexGuard};

pub(crate) use crate::common::error::NotAvailable;
use crate::system::poison::TryLockError;

pub(crate) struct Mutex<T>(LoomMutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub(crate) fn new(value: T) -> Self {
        Self(LoomMutex::new(value))
    }

    #[inline]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard {
            inner: Some(lock(&self.0)),
            lock: &self.0,
        }
    }

    #[inline]
    pub(crate) fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable> {
        let inner = match self.0.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(NotAvailable),
        };
        Ok(MutexGuard {
            inner: Some(inner),
            lock: &self.0,
        })
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub(crate) struct MutexGuard<'a, T> {
    pub(crate) inner: Option<LoomMutexGuard<'a, T>>,
    lock: &'a LoomMutex<T>,
}

impl<T> MutexGuard<'_, T> {
    #[inline]
    pub(crate) fn unlocked<F: FnOnce()>(&mut self, f: F) {
        let Some(inner) = self.inner.take() else {
            unreachable!("mutex guard is already unlocked");
        };
        drop(inner);

        let relock = Relock { guard: self };
        f();
        drop(relock);
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        let Some(inner) = &self.inner else {
            unreachable!("mutex guard is temporarily unlocked");
        };
        inner
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        let Some(inner) = &mut self.inner else {
            unreachable!("mutex guard is temporarily unlocked");
        };
        inner
    }
}

struct Relock<'guard, 'lock, T> {
    guard: &'guard mut MutexGuard<'lock, T>,
}

impl<T> Drop for Relock<'_, '_, T> {
    fn drop(&mut self) {
        self.guard.inner = Some(lock(self.guard.lock));
    }
}

fn lock<T>(mutex: &LoomMutex<T>) -> LoomMutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(error) => error.into_inner(),
    }
}
