use std::{
    fmt,
    ops::{Deref, DerefMut},
};

use kithara_platform::sync::Arc;

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
        self.value.take().expect(
            "BUG: PooledOwned inner value taken twice (Option::None implies prior into_inner)",
        )
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
    /// the pool's byte budget or its capacity cannot be reserved.
    ///
    /// # Panics
    ///
    /// Panics if the inner value has already been taken via [`into_inner()`](Self::into_inner).
    pub fn ensure_len(&mut self, min_len: usize) -> Result<(), BudgetExhausted> {
        let buf = self.value.as_mut().expect(
            "BUG: PooledOwned inner value taken twice (Option::None implies prior into_inner)",
        );
        if min_len <= buf.len() {
            return Ok(());
        }

        let old_cap = buf.capacity();
        if min_len <= old_cap {
            buf.resize(min_len, U::default());
            return Ok(());
        }

        let element_size = size_of::<U>();
        let old_bytes = old_cap.checked_mul(element_size).ok_or(BudgetExhausted)?;

        // Amortized doubling keeps repeated incremental growth O(n); when the
        // budget cannot afford the doubled capacity, retry with the exact fit.
        let amortized_cap = min_len.max(old_cap.saturating_mul(2));
        let mut grown = match reserve_charged(&self.pool, amortized_cap, old_bytes, element_size) {
            Ok(grown) => grown,
            Err(BudgetExhausted) if amortized_cap > min_len => {
                reserve_charged(&self.pool, min_len, old_bytes, element_size)?
            }
            Err(error) => return Err(error),
        };

        grown.append(buf);
        grown.resize(min_len, U::default());
        *buf = grown;
        Ok(())
    }
}

/// Reserve a fresh buffer of exactly `cap` elements with its capacity delta
/// over `old_bytes` charged to the pool budget. Any failure leaves the budget
/// unchanged.
fn reserve_charged<const SHARDS: usize, U>(
    pool: &Pool<SHARDS, Vec<U>>,
    cap: usize,
    old_bytes: usize,
    element_size: usize,
) -> Result<Vec<U>, BudgetExhausted>
where
    U: Default + Clone,
{
    let requested_bytes = cap.checked_mul(element_size).ok_or(BudgetExhausted)?;
    let requested_delta = requested_bytes - old_bytes;
    pool.request_budget(requested_delta)?;

    let mut grown = Vec::new();
    if grown.try_reserve_exact(cap).is_err() {
        pool.release_budget(requested_delta);
        return Err(BudgetExhausted);
    }

    let actual_bytes = grown
        .capacity()
        .checked_mul(element_size)
        .ok_or(BudgetExhausted);
    let actual_bytes = match actual_bytes {
        Ok(actual_bytes) => actual_bytes,
        Err(error) => {
            pool.release_budget(requested_delta);
            return Err(error);
        }
    };
    let actual_delta = actual_bytes - old_bytes;
    if actual_delta > requested_delta {
        if let Err(error) = pool.request_budget(actual_delta - requested_delta) {
            pool.release_budget(requested_delta);
            return Err(error);
        }
    } else {
        pool.release_budget(requested_delta - actual_delta);
    }

    Ok(grown)
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
        self.value.as_ref().expect(
            "BUG: PooledOwned inner value taken twice (Option::None implies prior into_inner)",
        )
    }
}

impl<const SHARDS: usize, T> DerefMut for PooledOwned<SHARDS, T>
where
    T: Reuse,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect(
            "BUG: PooledOwned inner value taken twice (Option::None implies prior into_inner)",
        )
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
