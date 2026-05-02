//! `PooledOwned` — `'static`-friendly RAII guard that holds `Arc<Pool>`.

use std::{
    fmt,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use super::{core::Pool, reuse::Reuse};
use crate::growth::BudgetExhausted;

/// Owned RAII wrapper for pooled buffer (holds `Arc<Pool>` instead of `&Pool`).
///
/// This version owns an Arc to the pool, making it `'static` and usable in
/// contexts like `async_stream::stream!` that require `'static` lifetimes.
pub struct PooledOwned<const SHARDS: usize, T>
where
    T: Reuse,
{
    pub(crate) pool: Arc<Pool<SHARDS, T>>,
    pub(crate) value: Option<T>,
    pub(crate) shard_idx: usize,
}

impl<const SHARDS: usize, T> PooledOwned<SHARDS, T>
where
    T: Reuse,
{
    /// Extract the value from the pool without returning it.
    ///
    /// # Panics
    ///
    /// Panics if the value was already taken (should not happen in normal use).
    pub fn into_inner(mut self) -> T {
        self.value.take().expect("PooledOwned value already taken")
    }

    pub(super) fn wrap(pool: Arc<Pool<SHARDS, T>>, value: T, shard_idx: usize) -> Self {
        Self {
            pool,
            shard_idx,
            value: Some(value),
        }
    }
}

impl<const SHARDS: usize, U> PooledOwned<SHARDS, Vec<U>>
where
    U: Default + Clone,
{
    /// Grow buffer to at least `min_len` elements. Budget-checked.
    ///
    /// No-op if buffer is already `>= min_len`. Charges the capacity delta
    /// (in bytes, `capacity × size_of::<U>()`) to the pool's byte budget.
    /// Returns `Err(BudgetExhausted)` if the budget would be exceeded
    /// (buffer is left unchanged in that case).
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExhausted`] if growing the buffer would exceed
    /// the pool's byte budget.
    ///
    /// # Panics
    ///
    /// Panics if the inner value has already been taken via [`into_inner()`](Self::into_inner).
    pub fn ensure_len(&mut self, min_len: usize) -> Result<(), BudgetExhausted> {
        let buf = self
            .value
            .as_mut()
            .expect("PooledOwned value already taken");
        if min_len <= buf.len() {
            return Ok(());
        }
        let old_cap = buf.capacity();
        buf.resize(min_len, U::default());
        let new_cap = buf.capacity();
        let byte_delta = (new_cap - old_cap) * size_of::<U>();
        if byte_delta > 0
            && let Err(e) = self.pool.request_budget(byte_delta)
        {
            // Budget exceeded — rollback the resize.
            buf.truncate(0);
            buf.shrink_to(old_cap);
            return Err(e);
        }
        Ok(())
    }
}

impl<const SHARDS: usize, T> Drop for PooledOwned<SHARDS, T>
where
    T: Reuse,
{
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            self.pool.put(value, self.shard_idx);
        }
    }
}

impl<const SHARDS: usize, T> Deref for PooledOwned<SHARDS, T>
where
    T: Reuse,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value
            .as_ref()
            .expect("PooledOwned value already taken")
    }
}

impl<const SHARDS: usize, T> DerefMut for PooledOwned<SHARDS, T>
where
    T: Reuse,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value
            .as_mut()
            .expect("PooledOwned value already taken")
    }
}

impl<const SHARDS: usize, T> fmt::Debug for PooledOwned<SHARDS, T>
where
    T: Reuse + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Some(v) => v.fmt(f),
            None => write!(f, "<taken>"),
        }
    }
}

impl<const SHARDS: usize, T> PartialEq for PooledOwned<SHARDS, T>
where
    T: Reuse + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
