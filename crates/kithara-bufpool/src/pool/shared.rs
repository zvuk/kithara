//! `SharedPool` — Arc-wrapped pool for shared access across components.

use std::{fmt, sync::Arc};

use super::{
    core::{Pool, PoolStats},
    owned::PooledOwned,
    reuse::Reuse,
};

/// Helper to create `Arc`-wrapped Pool for shared access.
///
/// Useful when pool needs to be shared across multiple components.
pub struct SharedPool<const SHARDS: usize, T>(Arc<Pool<SHARDS, T>>)
where
    T: Reuse;

impl<const SHARDS: usize, T> SharedPool<SHARDS, T>
where
    T: Reuse + Default,
{
    /// Create a new shared pool.
    #[must_use]
    pub fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        Self(Arc::new(Pool::new(max_buffers, trim_capacity)))
    }

    /// Get a buffer from the shared pool.
    #[must_use]
    pub fn get(&self) -> PooledOwned<SHARDS, T> {
        self.get_with(|_| {})
    }

    /// Get a buffer with initialization.
    pub fn get_with<F>(&self, init: F) -> PooledOwned<SHARDS, T>
    where
        F: FnOnce(&mut T),
    {
        let (value, shard_idx) = self.0.acquire(init);
        PooledOwned::wrap(Arc::clone(&self.0), value, shard_idx)
    }

    /// Pre-warm the pool by creating and recycling `count` buffers.
    ///
    /// Each buffer is initialized via `init`, then returned to the pool.
    /// This eliminates cold-start allocation misses for real-time paths.
    ///
    /// Respects byte budget — the `init` closure may trigger allocations
    /// tracked via `get_with`. Buffers that exceed budget are simply dropped.
    pub fn pre_warm<F: Fn(&mut T)>(&self, count: usize, init: F) {
        for _ in 0..count {
            let mut val = T::default();
            init(&mut val);
            let bytes = val.byte_size();
            // Stop if adding this buffer would exceed budget.
            if self.0.request_budget(bytes).is_err() {
                break;
            }
            self.0.recycle(val);
        }
    }

    /// Create a shared pool with a byte budget limit.
    ///
    /// See [`Pool::with_byte_budget()`] for details.
    #[must_use]
    pub fn with_byte_budget(max_buffers: usize, trim_capacity: usize, max_bytes: usize) -> Self {
        Self(Arc::new(Pool::with_byte_budget(
            max_buffers,
            trim_capacity,
            max_bytes,
        )))
    }
}

impl<const SHARDS: usize, T> SharedPool<SHARDS, T>
where
    T: Reuse,
{
    /// Current number of tracked bytes across all live buffers.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.0.allocated_bytes()
    }

    /// Wrap an externally-owned value into a [`PooledOwned`] guard.
    ///
    /// The returned guard automatically returns the value to this pool on drop,
    /// just like a value obtained via [`get()`](SharedPool::get).
    ///
    /// Useful for attaching pool-recycling to values that were extracted via
    /// [`PooledOwned::into_inner()`] or created outside the pool.
    pub fn attach(&self, value: T) -> PooledOwned<SHARDS, T> {
        let shard_idx = self.0.shard_index();
        PooledOwned::wrap(Arc::clone(&self.0), value, shard_idx)
    }

    /// Return a value to the pool for reuse.
    ///
    /// See [`Pool::recycle()`] for details.
    pub fn recycle(&self, value: T) {
        self.0.recycle(value);
    }

    /// Get pool hit/miss statistics.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        self.0.stats()
    }
}

impl<const SHARDS: usize, T> Clone for SharedPool<SHARDS, T>
where
    T: Reuse,
{
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<const SHARDS: usize, T> fmt::Debug for SharedPool<SHARDS, T>
where
    T: Reuse,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPool").finish_non_exhaustive()
    }
}
