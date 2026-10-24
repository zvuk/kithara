use std::ops::{Deref, DerefMut};

use wasm_safe_thread::{Mutex as WasmMutex, guard::Guard};

pub use crate::common::error::NotAvailable;

pub struct Mutex<T>(WasmMutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(WasmMutex::new(value))
    }

    delegate::delegate! {
        to self.0 {
            #[inline]
            #[expr(MutexGuard($))]
            #[call(lock_sync)]
            pub fn lock(&self) -> MutexGuard<'_, T>;
            /// Try to acquire the lock without blocking.
            ///
            /// # Errors
            ///
            /// Returns [`NotAvailable`] if the mutex is already held.
            #[inline]
            #[expr($.map(MutexGuard).map_err(|_| NotAvailable))]
            pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, NotAvailable>;
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// The inner guard is `pub(crate)` because the condvar implementation needs the raw guard.
pub struct MutexGuard<'a, T>(pub(crate) Guard<'a, T>);

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
    use std::rc::Rc;

    use super::Mutex;

    const _: bool = {
        trait AmbiguousIfSend<A> {
            const CHECK: bool;
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {
            const CHECK: bool = true;
        }
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {
            const CHECK: bool = true;
        }
        <Mutex<Rc<()>> as AmbiguousIfSend<_>>::CHECK
    };

    const _: bool = {
        trait AmbiguousIfSync<A> {
            const CHECK: bool;
        }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {
            const CHECK: bool = true;
        }
        impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {
            const CHECK: bool = true;
        }
        <Mutex<Rc<()>> as AmbiguousIfSync<_>>::CHECK
    };
}
