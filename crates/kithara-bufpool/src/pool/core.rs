//! `Pool` (sharded buffer pool) + `PoolStats`.

use std::{
    array,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use kithara_platform::{Mutex, thread::current_thread_id};

use super::{reuse::Reuse, shard::PoolShard};
use crate::growth::BudgetExhausted;

/// Pool hit/miss statistics for observability.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// No reusable buffer found — fresh allocation.
    pub alloc_misses: u64,
    /// Buffer retrieved from home shard (best case).
    pub home_hits: u64,
    /// Buffer dropped on return (shard full or reuse rejected).
    pub put_drops: u64,
    /// Buffer stolen from a non-home shard.
    pub steal_hits: u64,
    /// Current tracked byte budget usage.
    pub allocated_bytes: usize,
}

/// Generic sharded buffer pool.
///
/// Type parameters:
/// - `SHARDS`: Number of shards (compile-time constant for optimal performance)
/// - `T`: Type of buffer (must implement `Reuse`)
///
/// ## Sharding
///
/// The pool is divided into multiple shards to reduce lock contention.
/// Each thread gets assigned to a shard based on its thread ID.
///
/// ## Memory Management
///
/// - `max_buffers`: Maximum buffers across all shards
/// - `trim_capacity`: Shrink buffers to this size when returning to pool
pub struct Pool<const SHARDS: usize, T>
where
    T: Reuse,
{
    pub(super) shards: [Mutex<PoolShard<T>>; SHARDS],
    stat_alloc_misses: AtomicU64,
    stat_home_hits: AtomicU64,
    stat_put_drops: AtomicU64,
    stat_steal_hits: AtomicU64,
    /// Total bytes tracked across all live buffers (pooled + checked out).
    allocated_bytes: AtomicUsize,
    /// Maximum allowed byte budget. `usize::MAX` means unlimited.
    max_bytes: usize,
}

impl<const SHARDS: usize, T> Pool<SHARDS, T>
where
    T: Reuse,
{
    /// Maximum number of non-home shards to probe on a miss.
    ///
    /// Uses `try_lock` to avoid blocking — locked shards are skipped.
    const MAX_PROBE: usize = 4;

    /// Current number of tracked bytes across all live buffers.
    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes.load(Ordering::Relaxed)
    }

    /// Return a buffer to the pool.
    pub(crate) fn put(&self, value: T, shard_idx: usize) {
        let bytes = value.byte_size();
        let mut shard = self.shards[shard_idx].lock_sync();
        if !shard.try_put(value) {
            // Shard full or buffer rejected — release tracked bytes.
            drop(shard);
            self.release_budget(bytes);
            self.stat_put_drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Release byte budget (e.g., when a buffer is dropped without returning to pool).
    ///
    /// Uses saturating subtraction to prevent underflow when buffers grow
    /// via `DerefMut` (e.g., `Vec::resize`) without going through
    /// [`super::owned::PooledOwned::ensure_len`].
    pub fn release_budget(&self, amount: usize) {
        if amount == 0 {
            return;
        }
        let mut current = self.allocated_bytes.load(Ordering::Relaxed);
        loop {
            let new = current.saturating_sub(amount);
            match self.allocated_bytes.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Request additional byte budget. Returns `Err` if exceeding `max_bytes`.
    ///
    /// Uses a compare-and-swap loop to atomically check and update.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExhausted`] if adding `additional` bytes would exceed
    /// the pool's `max_bytes` limit, or if the total would overflow `usize`.
    pub fn request_budget(&self, additional: usize) -> Result<(), BudgetExhausted> {
        if additional == 0 {
            return Ok(());
        }
        let mut current = self.allocated_bytes.load(Ordering::Relaxed);
        loop {
            let new = current.checked_add(additional).ok_or(BudgetExhausted)?;
            if new > self.max_bytes {
                return Err(BudgetExhausted);
            }
            match self.allocated_bytes.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    /// Determine shard index for current thread. Pure function of the
    /// `SHARDS` const; takes no `self`. Result is bounded by `SHARDS`,
    /// which fits in `usize` on every platform we target — so the
    /// `usize::try_from` here is infallible in practice and the
    /// `.unwrap_or(0)` is just a non-panicking fallback.
    #[inline]
    pub(crate) fn shard_index() -> usize {
        let tid = current_thread_id();
        let shards_u64 = SHARDS as u64;
        usize::try_from(tid % shards_u64).unwrap_or(0)
    }

    /// Get pool hit/miss statistics.
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            alloc_misses: self.stat_alloc_misses.load(Ordering::Relaxed),
            allocated_bytes: self.allocated_bytes.load(Ordering::Relaxed),
            home_hits: self.stat_home_hits.load(Ordering::Relaxed),
            put_drops: self.stat_put_drops.load(Ordering::Relaxed),
            steal_hits: self.stat_steal_hits.load(Ordering::Relaxed),
        }
    }

    /// Track byte delta without enforcement (for `get_with` closures).
    ///
    /// When shrinking (before > after), uses saturating subtraction
    /// to prevent underflow from untracked external growth.
    fn track_byte_delta(&self, before: usize, after: usize) {
        if after > before {
            self.allocated_bytes
                .fetch_add(after - before, Ordering::Relaxed);
        } else if before > after {
            self.release_budget(before - after);
        }
    }

    /// Try to steal a buffer from nearby shards using non-blocking `try_lock`.
    ///
    /// Probes up to [`MAX_PROBE`](Self::MAX_PROBE) shards starting from `home + 1`.
    /// Skips shards whose lock is currently held.
    fn try_steal(&self, home: usize) -> Option<T> {
        let probe = Self::MAX_PROBE.min(SHARDS.saturating_sub(1));
        for i in 1..=probe {
            let idx = (home + i) % SHARDS;
            if let Ok(mut shard) = self.shards[idx].try_lock()
                && let Some(v) = shard.try_get()
            {
                return Some(v);
            }
        }
        None
    }
}

impl<const SHARDS: usize, T> Pool<SHARDS, T>
where
    T: Reuse,
{
    /// Return a value to the pool for reuse.
    ///
    /// Useful for returning buffers that were extracted via
    /// [`super::pooled::Pooled::into_inner`] or
    /// [`super::owned::PooledOwned::into_inner`] after the caller is done.
    ///
    /// The value is cleared and trimmed via [`Reuse::reuse`] before storing.
    /// If the pool is full, the value is silently dropped.
    pub fn recycle(&self, value: T) {
        let shard_idx = Self::shard_index();
        self.put(value, shard_idx);
    }
}

impl<const SHARDS: usize, T> Pool<SHARDS, T>
where
    T: Reuse + Default,
{
    /// Create a new pool.
    ///
    /// # Arguments
    ///
    /// - `max_buffers`: Maximum total buffers across all shards
    /// - `trim_capacity`: Shrink buffers to this capacity when returning
    ///
    /// # Panics
    ///
    /// Panics if `SHARDS` is zero.
    ///
    /// # Example
    ///
    /// ```
    /// use kithara_bufpool::Pool;
    ///
    /// // Pool for Vec<u8> with 1024 max buffers, trim to 128KB
    /// let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
    /// ```
    #[must_use]
    pub fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        Self::with_byte_budget(max_buffers, trim_capacity, usize::MAX)
    }

    /// Shared implementation for `Pool::get_with` and `SharedPool::get_with`:
    /// picks a shard, tries home → steal → alloc, updates stats, runs init,
    /// tracks byte delta. Returns `(value, shard_idx)` so callers can wrap
    /// it in `Pooled<'_>` or `PooledOwned` depending on ownership.
    pub(crate) fn acquire<F>(&self, init: F) -> (T, usize)
    where
        F: FnOnce(&mut T),
    {
        let shard_idx = Self::shard_index();
        let mut value = {
            let mut shard = self.shards[shard_idx].lock_sync();
            shard.try_get()
        };

        if value.is_some() {
            self.stat_home_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            value = self.try_steal(shard_idx);
            if value.is_some() {
                self.stat_steal_hits.fetch_add(1, Ordering::Relaxed);
            } else {
                self.stat_alloc_misses.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut value = value.unwrap_or_default();
        let before = value.byte_size();
        init(&mut value);
        let after = value.byte_size();
        self.track_byte_delta(before, after);

        (value, shard_idx)
    }

    /// Get a buffer from the pool.
    ///
    /// If pool has available buffers, returns a reused one.
    /// Otherwise, creates a new default buffer.
    ///
    /// The buffer is automatically returned to the pool on drop.
    pub fn get(&self) -> super::pooled::Pooled<'_, SHARDS, T> {
        self.get_with(|_| {})
    }

    /// Get a buffer and apply initialization function.
    ///
    /// Useful for setting up the buffer after getting it from pool.
    ///
    /// # Example
    ///
    /// ```
    /// use kithara_bufpool::Pool;
    ///
    /// let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
    /// let buf = pool.get_with(|b| b.resize(1024, 0));
    /// assert_eq!(buf.len(), 1024);
    /// ```
    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub fn get_with<F>(&self, init: F) -> super::pooled::Pooled<'_, SHARDS, T>
    where
        F: FnOnce(&mut T),
    {
        let (value, shard_idx) = self.acquire(init);
        super::pooled::Pooled::wrap(self, value, shard_idx)
    }

    /// Create a pool with a byte budget limit.
    ///
    /// - `max_buffers`: Maximum total buffers across all shards.
    /// - `trim_capacity`: Shrink buffers to this capacity when returning.
    /// - `max_bytes`: Maximum total bytes tracked. `usize::MAX` = unlimited.
    ///
    /// # Panics
    ///
    /// Panics if `SHARDS` is zero.
    #[must_use]
    pub fn with_byte_budget(max_buffers: usize, trim_capacity: usize, max_bytes: usize) -> Self {
        assert!(SHARDS > 0, "Pool must have at least 1 shard");
        let buffers_per_shard = max_buffers / SHARDS;

        Self {
            max_bytes,
            allocated_bytes: AtomicUsize::new(0),
            shards: array::from_fn(|_| {
                Mutex::new(PoolShard::new(buffers_per_shard, trim_capacity))
            }),
            stat_alloc_misses: AtomicU64::new(0),
            stat_home_hits: AtomicU64::new(0),
            stat_put_drops: AtomicU64::new(0),
            stat_steal_hits: AtomicU64::new(0),
        }
    }
}
