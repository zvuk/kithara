use std::{
    array, fmt,
    ops::{Deref, DerefMut},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_platform::Mutex;

use crate::growth::BudgetExhausted;

/// Trait for types that can be reused in a pool.
///
/// Implementors must provide logic to clear/reset the value
/// and optionally shrink capacity to a trim size.
#[cfg_attr(test, unimock::unimock(api = ReuseMock))]
pub trait Reuse {
    /// Prepare this value for reuse.
    ///
    /// Should clear the contents and optionally shrink capacity
    /// to the specified trim size to prevent unbounded growth.
    ///
    /// Returns `true` if the value still has capacity and can be reused,
    /// `false` if it should be dropped.
    fn reuse(&mut self, trim: usize) -> bool;

    /// Returns the number of bytes this value occupies in memory.
    ///
    /// Used by the pool's byte budget tracking. Default returns 0
    /// (no budget tracking for types that don't override this).
    fn byte_size(&self) -> usize {
        0
    }
}

/// Reuse implementation for `Vec<T>`.
///
/// Clears the vector and shrinks capacity to trim size.
impl<T> Reuse for Vec<T> {
    fn reuse(&mut self, trim: usize) -> bool {
        self.clear();
        self.shrink_to(trim);
        self.capacity() > 0
    }

    fn byte_size(&self) -> usize {
        self.capacity() * size_of::<T>()
    }
}

/// A single shard in the pool.
struct PoolShard<T> {
    /// Buffers available for reuse.
    buffers: Vec<T>,
    /// Maximum number of buffers in this shard.
    max_buffers: usize,
    /// Trim capacity to this size when returning to pool.
    trim_capacity: usize,
}

impl<T> PoolShard<T>
where
    T: Reuse,
{
    fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        Self {
            buffers: Vec::with_capacity(max_buffers.min(16)),
            max_buffers,
            trim_capacity,
        }
    }

    /// Try to get a buffer from this shard.
    fn try_get(&mut self) -> Option<T> {
        self.buffers.pop()
    }

    /// Try to return a buffer to this shard.
    fn try_put(&mut self, mut value: T) -> bool {
        if self.buffers.len() >= self.max_buffers {
            return false; // Shard full, drop the buffer
        }

        if value.reuse(self.trim_capacity) {
            self.buffers.push(value);
            true
        } else {
            false // Capacity dropped to zero, don't reuse
        }
    }
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
    shards: [Mutex<PoolShard<T>>; SHARDS],
    /// Total bytes tracked across all live buffers (pooled + checked out).
    allocated_bytes: AtomicUsize,
    /// Maximum allowed byte budget. `usize::MAX` means unlimited.
    max_bytes: usize,
}

impl<const SHARDS: usize, T> Pool<SHARDS, T>
where
    T: Reuse,
{
    /// Determine shard index for current thread.
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "method on Pool for API consistency; may use self in future for per-pool salt"
    )]
    pub(crate) fn shard_index(&self) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "modulo SHARDS guarantees result fits in usize"
        )]
        let idx = (kithara_platform::thread::current_thread_id() as usize) % SHARDS;
        idx
    }

    /// Return a buffer to the pool.
    pub(crate) fn put(&self, value: T, shard_idx: usize) {
        let bytes = value.byte_size();
        let mut shard = self.shards[shard_idx].lock_sync();
        if !shard.try_put(value) {
            // Shard full or buffer rejected — release tracked bytes.
            drop(shard);
            self.release_budget(bytes);
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

    /// Release byte budget (e.g., when a buffer is dropped without returning to pool).
    ///
    /// Uses saturating subtraction to prevent underflow when buffers grow
    /// via `DerefMut` (e.g., `Vec::resize`) without going through
    /// [`ensure_len()`](PooledOwned::ensure_len).
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

    /// Current number of tracked bytes across all live buffers.
    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes.load(Ordering::Relaxed)
    }
}

impl<const SHARDS: usize, T> Pool<SHARDS, T>
where
    T: Reuse,
{
    /// Return a value to the pool for reuse.
    ///
    /// Useful for returning buffers that were extracted via [`Pooled::into_inner()`]
    /// or [`PooledOwned::into_inner()`] after the caller is done with them.
    ///
    /// The value is cleared and trimmed via [`Reuse::reuse()`] before storing.
    /// If the pool is full, the value is silently dropped.
    pub fn recycle(&self, value: T) {
        let shard_idx = self.shard_index();
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
            shards: array::from_fn(|_| {
                Mutex::new(PoolShard::new(buffers_per_shard, trim_capacity))
            }),
            allocated_bytes: AtomicUsize::new(0),
            max_bytes,
        }
    }

    /// Get a buffer from the pool.
    ///
    /// If pool has available buffers, returns a reused one.
    /// Otherwise, creates a new default buffer.
    ///
    /// The buffer is automatically returned to the pool on drop.
    pub fn get(&self) -> Pooled<'_, SHARDS, T> {
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
    pub fn get_with<F>(&self, init: F) -> Pooled<'_, SHARDS, T>
    where
        F: FnOnce(&mut T),
    {
        let shard_idx = self.shard_index();
        let mut value = {
            let mut shard = self.shards[shard_idx].lock_sync();
            shard.try_get()
        };

        if value.is_none() {
            // Try other shards before allocating
            for i in 1..SHARDS {
                let idx = (shard_idx + i) % SHARDS;
                let mut shard = self.shards[idx].lock_sync();
                if let Some(v) = shard.try_get() {
                    value = Some(v);
                    break;
                }
            }
        }

        let mut value = value.unwrap_or_default();
        let before = value.byte_size();
        init(&mut value);
        let after = value.byte_size();
        self.track_byte_delta(before, after);

        Pooled {
            value: Some(value),
            pool: self,
            shard_idx,
        }
    }
}

/// RAII wrapper for pooled buffer.
///
/// Automatically returns the buffer to the pool on drop.
pub struct Pooled<'a, const SHARDS: usize, T>
where
    T: Reuse,
{
    value: Option<T>,
    pool: &'a Pool<SHARDS, T>,
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

/// Owned RAII wrapper for pooled buffer (holds `Arc<Pool>` instead of `&Pool`).
///
/// This version owns an Arc to the pool, making it `'static` and usable in
/// contexts like `async_stream::stream!` that require `'static` lifetimes.
pub struct PooledOwned<const SHARDS: usize, T>
where
    T: Reuse,
{
    pub(crate) value: Option<T>,
    pub(crate) pool: Arc<Pool<SHARDS, T>>,
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
}

impl<const SHARDS: usize> PooledOwned<SHARDS, Vec<u8>> {
    /// Grow buffer to at least `min_len` bytes (zeroed). Budget-checked.
    ///
    /// No-op if buffer is already `>= min_len`. Charges the capacity delta
    /// to the pool's byte budget. Returns `Err(BudgetExhausted)` if the
    /// budget would be exceeded (buffer is left unchanged in that case).
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
        buf.resize(min_len, 0);
        let new_cap = buf.capacity();
        if new_cap > old_cap
            && let Err(e) = self.pool.request_budget(new_cap - old_cap)
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
            Some(v) => fmt::Debug::fmt(v, f),
            None => write!(f, "<taken>"),
        }
    }
}

impl<const SHARDS: usize, T> Clone for PooledOwned<SHARDS, T>
where
    T: Reuse + Clone,
{
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            pool: Arc::clone(&self.pool),
            shard_idx: self.shard_idx,
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
        let shard_idx = self.0.shard_index();
        let mut value = {
            let mut shard = self.0.shards[shard_idx].lock_sync();
            shard.try_get()
        };

        if value.is_none() {
            // Try other shards before allocating
            for i in 1..SHARDS {
                let idx = (shard_idx + i) % SHARDS;
                let mut shard = self.0.shards[idx].lock_sync();
                if let Some(v) = shard.try_get() {
                    value = Some(v);
                    break;
                }
            }
        }

        let mut value = value.unwrap_or_default();
        let before = value.byte_size();
        init(&mut value);
        let after = value.byte_size();
        self.0.track_byte_delta(before, after);

        PooledOwned {
            value: Some(value),
            pool: Arc::clone(&self.0),
            shard_idx,
        }
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

    /// Return a value to the pool for reuse.
    ///
    /// See [`Pool::recycle()`] for details.
    pub fn recycle(&self, value: T) {
        self.0.recycle(value);
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
        PooledOwned {
            value: Some(value),
            pool: Arc::clone(&self.0),
            shard_idx,
        }
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
