use std::{
    ops::{Deref, DerefMut},
    panic::Location,
    sync::Arc,
};

pub use crate::native::sync::mutex::NotAvailable;
use crate::{
    flash::diag::{self, PrimEntry, PrimKind},
    native::sync::mutex::{Mutex as NativeMutex, MutexGuard as NativeGuard},
};

/// `Mutex` under `flash`: a real `parking_lot` lock — flash-blind to the
/// quiescence engine/clock by contract (bounded critical sections) — wrapped so
/// the diagnostics registry can see who created it, who holds it, and who is
/// blocked acquiring it. Observation only: blocking semantics, lock order and
/// fairness are unchanged. Tracking is live only when `KITHARA_FLASH_SYNC_TRACE`
/// is set; otherwise `meta` is `None` and the hot path is a single null check.
pub struct Mutex<T> {
    inner: NativeMutex<T>,
    meta: Option<Arc<PrimEntry>>,
}

impl<T> Mutex<T> {
    #[track_caller]
    #[inline]
    pub fn new(value: T) -> Self {
        let meta = diag::register(PrimKind::Mutex, None, Location::caller());
        Self {
            meta,
            inner: NativeMutex::new(value),
        }
    }

    #[track_caller]
    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let at = Location::caller();
        if let Some(m) = &self.meta {
            m.enter_pending(at);
        }
        let inner = self.inner.lock();
        if let Some(m) = &self.meta {
            m.acquired(at);
        }
        MutexGuard {
            inner,
            at,
            meta: self.meta.as_deref(),
        }
    }

    /// Try to acquire the lock without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the mutex is already held.
    #[track_caller]
    #[inline]
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable> {
        let at = Location::caller();
        let inner = self.inner.try_lock()?;
        if let Some(m) = &self.meta {
            m.acquired(at);
        }
        Ok(MutexGuard {
            inner,
            at,
            meta: self.meta.as_deref(),
        })
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// Guard for [`Mutex`]. Clears the holder record on drop and brackets the
/// `unlocked` window so the registry never shows a released lock as held.
pub struct MutexGuard<'a, T> {
    at: &'static Location<'static>,
    inner: NativeGuard<'a, T>,
    meta: Option<&'a PrimEntry>,
}

impl<'a, T> MutexGuard<'a, T> {
    /// The diagnostics entry, if tracing, for holder bookkeeping across a wait.
    #[inline]
    pub(in crate::flash) fn meta(&self) -> Option<&'a PrimEntry> {
        self.meta
    }

    /// The inner native guard, for the flash [`Condvar`](super::Condvar) native
    /// arm's in-place wait (which must not move the guard — `Drop` forbids it).
    #[inline]
    pub(in crate::flash) fn native_mut(&mut self) -> &mut NativeGuard<'a, T> {
        &mut self.inner
    }

    /// The site this guard's lock was acquired at — reused as the re-acquire
    /// site after a condvar wait re-takes the lock.
    #[inline]
    pub(in crate::flash) fn site(&self) -> &'static Location<'static> {
        self.at
    }

    /// Temporarily unlock the mutex, run `f`, then relock before returning.
    ///
    /// The data may change while unlocked: re-validate any predicate after
    /// the call (classic condvar-style usage).
    #[inline]
    pub fn unlocked<F: FnOnce()>(&mut self, f: F) {
        if let Some(m) = self.meta {
            m.released();
        }
        self.inner.unlocked(f);
        if let Some(m) = self.meta {
            m.acquired(self.at);
        }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(m) = self.meta {
            m.released();
        }
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}
