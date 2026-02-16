//! WASM-safe synchronization primitives.
//!
//! Wraps `parking_lot` types to avoid `Atomics.wait()` on the browser main
//! thread. All `lock()` / `read()` / `write()` methods use `try_lock()` in
//! a spin loop instead of the blocking path.

use std::{
    fmt,
    ops::{Deref, DerefMut},
};

/// WASM-safe mutex. Uses `try_lock()` + spin instead of blocking `lock()`.
pub struct Mutex<T: ?Sized>(parking_lot::Mutex<T>);

impl<T> Mutex<T> {
    /// Create a new mutex wrapping `value`.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self(parking_lot::Mutex::new(value))
    }

    /// Consume the mutex and return the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquire the lock using a spin loop (no `Atomics.wait`).
    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard(spin_lock(&self.0))
    }

    /// Try to acquire the lock without blocking.
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.0.try_lock().map(MutexGuard)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.try_lock() {
            Some(guard) => f.debug_tuple("Mutex").field(&&*guard).finish(),
            None => f.debug_tuple("Mutex").field(&"<locked>").finish(),
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// RAII guard for [`Mutex`].
pub struct MutexGuard<'a, T: ?Sized>(parking_lot::MutexGuard<'a, T>);

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// WASM-safe reader-writer lock. Uses spin loops for all locking paths.
pub struct RwLock<T: ?Sized>(parking_lot::RwLock<T>);

impl<T> RwLock<T> {
    /// Create a new reader-writer lock wrapping `value`.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self(parking_lot::RwLock::new(value))
    }

    /// Consume the lock and return the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Acquire a shared (read) lock using a spin loop.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard(spin_read(&self.0))
    }

    /// Acquire an exclusive (write) lock using a spin loop.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard(spin_write(&self.0))
    }

    /// Try to acquire a shared (read) lock without blocking.
    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.0.try_read().map(RwLockReadGuard)
    }

    /// Try to acquire an exclusive (write) lock without blocking.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.0.try_write().map(RwLockWriteGuard)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.try_read() {
            Some(guard) => f.debug_tuple("RwLock").field(&&*guard).finish(),
            None => f.debug_tuple("RwLock").field(&"<locked>").finish(),
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// RAII read guard for [`RwLock`].
pub struct RwLockReadGuard<'a, T: ?Sized>(parking_lot::RwLockReadGuard<'a, T>);

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// RAII write guard for [`RwLock`].
pub struct RwLockWriteGuard<'a, T: ?Sized>(parking_lot::RwLockWriteGuard<'a, T>);

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// WASM-safe condition variable.
///
/// Works with [`MutexGuard`] from this crate. The `wait` and `notify`
/// operations delegate to the inner `parking_lot::Condvar`, but accept our
/// wrapper guard type.
pub struct Condvar(parking_lot::Condvar);

impl Condvar {
    /// Create a new condition variable.
    #[inline]
    pub const fn new() -> Self {
        Self(parking_lot::Condvar::new())
    }

    /// Block until notified.
    ///
    /// The guard is temporarily released while waiting, then re-acquired
    /// (via spin loop) before returning.
    #[inline]
    pub fn wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        // SAFETY: `MutexGuard` wraps a `parking_lot::MutexGuard` that owns
        // the same underlying raw mutex. `parking_lot::Condvar::wait` will
        // unlock/relock via the raw mutex pointer, so this is sound.
        //
        // Note: `parking_lot::Condvar::wait` re-acquires via the blocking
        // path internally. On a Web Worker this is fine (Atomics.wait is
        // allowed). On the main thread, Condvar::wait should NOT be called
        // — the main thread uses async I/O, not condvar-based blocking.
        // Condvar::wait is only used in `Resource::wait_range` which runs
        // on the decoder's rayon worker thread.
        self.0.wait(&mut guard.0);
    }

    /// Block until notified or timeout elapses.
    ///
    /// Returns `true` if notified before timeout.
    #[inline]
    pub fn wait_for<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: std::time::Duration,
    ) -> bool {
        !self.0.wait_for(&mut guard.0, timeout).timed_out()
    }

    /// Wake one waiting thread.
    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    /// Wake all waiting threads.
    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Condvar").finish()
    }
}

// IMPORTANT: Do NOT use `std::hint::spin_loop()` here!
// On wasm32+atomics it compiles to `memory.atomic.wait32` (= `Atomics.wait`)
// which throws `TypeError` on the browser main thread regardless of timeout.
//
// A bare `try_lock()` loop is fine: contention windows are microseconds
// (the rayon worker holds locks briefly), so the loop resolves in 1-2
// iterations. This matches rayon's own `web_spin_lock` approach.

#[inline]
fn spin_lock<T: ?Sized>(m: &parking_lot::Mutex<T>) -> parking_lot::MutexGuard<'_, T> {
    loop {
        if let Some(guard) = m.try_lock() {
            return guard;
        }
    }
}

#[inline]
fn spin_read<T: ?Sized>(rw: &parking_lot::RwLock<T>) -> parking_lot::RwLockReadGuard<'_, T> {
    loop {
        if let Some(guard) = rw.try_read() {
            return guard;
        }
    }
}

#[inline]
fn spin_write<T: ?Sized>(rw: &parking_lot::RwLock<T>) -> parking_lot::RwLockWriteGuard<'_, T> {
    loop {
        if let Some(guard) = rw.try_write() {
            return guard;
        }
    }
}
