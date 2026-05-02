//! `Pooled<'a>` — RAII guard that borrows from a `Pool`.

use std::ops::{Deref, DerefMut};

use super::{core::Pool, reuse::Reuse};

/// RAII wrapper for pooled buffer.
///
/// Automatically returns the buffer to the pool on drop.
pub struct Pooled<'a, const SHARDS: usize, T>
where
    T: Reuse,
{
    pool: &'a Pool<SHARDS, T>,
    value: Option<T>,
    shard_idx: usize,
}

impl<'a, const SHARDS: usize, T> Pooled<'a, SHARDS, T>
where
    T: Reuse,
{
    /// Extract the value from the pool without returning it.
    ///
    /// This consumes the `Pooled` wrapper and returns the inner value.
    /// The value will not be returned to the pool.
    ///
    /// # Panics
    ///
    /// Panics if the value was already taken (should not happen in normal use).
    ///
    /// # Example
    ///
    /// ```
    /// use kithara_bufpool::Pool;
    ///
    /// let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
    /// let buf = pool.get_with(|b| b.extend_from_slice(b"hello"));
    /// let vec = buf.into_inner(); // Extract Vec, won't return to pool
    /// assert_eq!(&vec[..], b"hello");
    /// ```
    pub fn into_inner(mut self) -> T {
        self.value.take().expect("Pooled value already taken")
    }

    pub(super) fn wrap(pool: &'a Pool<SHARDS, T>, value: T, shard_idx: usize) -> Self {
        Self {
            pool,
            shard_idx,
            value: Some(value),
        }
    }
}

impl<const SHARDS: usize, T> Drop for Pooled<'_, SHARDS, T>
where
    T: Reuse,
{
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            self.pool.put(value, self.shard_idx);
        }
    }
}

impl<const SHARDS: usize, T> Deref for Pooled<'_, SHARDS, T>
where
    T: Reuse,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().expect("Pooled value already taken")
    }
}

impl<const SHARDS: usize, T> DerefMut for Pooled<'_, SHARDS, T>
where
    T: Reuse,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("Pooled value already taken")
    }
}
