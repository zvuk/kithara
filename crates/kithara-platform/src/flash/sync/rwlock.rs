use std::{
    ops::{Deref, DerefMut},
    panic::Location,
    sync::Arc,
};

use crate::{
    flash::diag::{self, PrimEntry, PrimKind},
    native::sync::rwlock::{
        RwLock as NativeRwLock, RwLockReadGuard as NativeReadGuard,
        RwLockWriteGuard as NativeWriteGuard,
    },
};

/// `RwLock` under `flash`: a real `parking_lot` lock — flash-blind to the engine
/// by contract — wrapped so the diagnostics registry sees its creation site, the
/// current writer (and where it locked), the live reader count, and any thread
/// blocked acquiring it. Observation only. Tracking is live only when
/// `KITHARA_FLASH_SYNC_TRACE` is set; otherwise `meta` is `None`.
pub struct RwLock<T> {
    inner: NativeRwLock<T>,
    meta: Option<Arc<PrimEntry>>,
}

impl<T> RwLock<T> {
    #[track_caller]
    #[inline]
    pub fn new(value: T) -> Self {
        let meta = diag::register(PrimKind::RwLock, None, Location::caller());
        Self {
            meta,
            inner: NativeRwLock::new(value),
        }
    }

    #[track_caller]
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        let at = Location::caller();
        if let Some(m) = &self.meta {
            m.enter_pending(at);
        }
        let inner = self.inner.read();
        if let Some(m) = &self.meta {
            m.read_acquired(at);
        }
        RwLockReadGuard {
            inner,
            meta: self.meta.as_deref(),
        }
    }

    #[track_caller]
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        let at = Location::caller();
        if let Some(m) = &self.meta {
            m.enter_pending(at);
        }
        let inner = self.inner.write();
        if let Some(m) = &self.meta {
            m.acquired(at);
        }
        RwLockWriteGuard {
            inner,
            meta: self.meta.as_deref(),
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub struct RwLockReadGuard<'a, T> {
    inner: NativeReadGuard<'a, T>,
    meta: Option<&'a PrimEntry>,
}

impl<T> Drop for RwLockReadGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(m) = self.meta {
            m.read_released();
        }
    }
}

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

pub struct RwLockWriteGuard<'a, T> {
    inner: NativeWriteGuard<'a, T>,
    meta: Option<&'a PrimEntry>,
}

impl<T> Drop for RwLockWriteGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(m) = self.meta {
            m.released();
        }
    }
}

impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}
