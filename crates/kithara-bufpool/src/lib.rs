//! Generic sharded buffer pool for zero-allocation hot paths.
//!
//! Provides a thread-safe, generic buffer pool with sharding to reduce contention.
//! Supports any type T that implements the `Reuse` trait.
//!
//! ## Features
//!
//! - **Generic**: Works with `Vec<u8>`, `Vec<f32>`, `Vec<f64>`, etc.
//! - **Sharded**: Reduces lock contention via multiple shards
//! - **RAII**: Buffers automatically return to pool on drop
//! - **Zero-copy**: Reuses allocated memory
//!
//! ## Example
//!
//! ```
//! use kithara_bufpool::{Pool, Reuse};
//! use std::sync::OnceLock;
//!
//! static POOL: OnceLock<&'static Pool<32, Vec<f32>>> = OnceLock::new();
//!
//! fn get_pool() -> &'static Pool<32, Vec<f32>> {
//!     *POOL.get_or_init(|| {
//!         let pool = Pool::<32, Vec<f32>>::new(1024, 128 * 1024);
//!         Box::leak(Box::new(pool))
//!     })
//! }
//!
//! // Get buffer from pool
//! let mut buf = get_pool().get();
//! buf.resize(1024, 0.0);
//! // Use buf...
//! // Automatically returned to pool on drop
//! ```

#![forbid(unsafe_code)]

use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

use parking_lot::Mutex;

/// Trait for types that can be reused in a pool.
///
/// Implementors must provide logic to clear/reset the value
/// and optionally shrink capacity to a trim size.
pub trait Reuse {
    /// Prepare this value for reuse.
    ///
    /// Should clear the contents and optionally shrink capacity
    /// to the specified trim size to prevent unbounded growth.
    ///
    /// Returns `true` if the value still has capacity and can be reused,
    /// `false` if it should be dropped.
    fn reuse(&mut self, trim: usize) -> bool;
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
}

impl<const SHARDS: usize, T> Pool<SHARDS, T>
where
    T: Reuse,
{
    /// Determine shard index for current thread.
    ///
    /// Uses thread ID for even distribution across shards.
    #[inline]
    fn shard_index(&self) -> usize {
        // Use thread ID as hash for shard selection
        let thread_id = std::thread::current().id();
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            thread_id.hash(&mut hasher);
            hasher.finish()
        };
        (hash as usize) % SHARDS
    }

    /// Return a buffer to the pool.
    ///
    /// Called automatically by `Pooled::drop`.
    fn put(&self, value: T, shard_idx: usize) {
        let mut shard = self.shards[shard_idx].lock();
        if !shard.try_put(value) {
            // Shard full or buffer rejected, drop it
        }
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
    /// # Example
    ///
    /// ```
    /// use kithara_bufpool::Pool;
    ///
    /// // Pool for Vec<u8> with 1024 max buffers, trim to 128KB
    /// let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
    /// ```
    pub fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        assert!(SHARDS > 0, "Pool must have at least 1 shard");
        let buffers_per_shard = max_buffers / SHARDS;

        Self {
            shards: std::array::from_fn(|_| {
                Mutex::new(PoolShard::new(buffers_per_shard, trim_capacity))
            }),
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
    pub fn get_with<F>(&self, init: F) -> Pooled<'_, SHARDS, T>
    where
        F: FnOnce(&mut T),
    {
        let shard_idx = self.shard_index();
        let mut value = {
            let mut shard = self.shards[shard_idx].lock();
            shard.try_get()
        };

        if value.is_none() {
            // Try other shards before allocating
            for i in 1..SHARDS {
                let idx = (shard_idx + i) % SHARDS;
                let mut shard = self.shards[idx].lock();
                if let Some(v) = shard.try_get() {
                    value = Some(v);
                    break;
                }
            }
        }

        let mut value = value.unwrap_or_default();
        init(&mut value);

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

/// Owned RAII wrapper for pooled buffer (holds Arc<Pool> instead of &Pool).
///
/// This version owns an Arc to the pool, making it 'static and usable in
/// contexts like async_stream::stream! that require 'static lifetimes.
pub struct PooledOwned<const SHARDS: usize, T>
where
    T: Reuse,
{
    value: Option<T>,
    pool: Arc<Pool<SHARDS, T>>,
    shard_idx: usize,
}

impl<const SHARDS: usize, T> PooledOwned<SHARDS, T>
where
    T: Reuse,
{
    /// Extract the value from the pool without returning it.
    pub fn into_inner(mut self) -> T {
        self.value.take().expect("PooledOwned value already taken")
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

/// Wrapper for pooled Vec with tracked length.
///
/// Holds a pooled buffer and tracks the actual length of valid data,
/// which may be less than the buffer's capacity.
///
/// Useful when reading data into a pooled buffer where the amount read
/// is not known in advance.
///
/// # Example
///
/// ```
/// use kithara_bufpool::{Pool, PooledSlice};
///
/// let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
/// let mut buf = pool.get_with(|b| b.resize(4096, 0));
///
/// // Simulate reading data (e.g., 100 bytes actually read)
/// let bytes_read = 100;
/// let slice = PooledSlice::new(buf, bytes_read);
///
/// assert_eq!(slice.as_slice().len(), 100);
/// ```
pub struct PooledSlice<'a, const SHARDS: usize, T> {
    buf: Pooled<'a, SHARDS, Vec<T>>,
    len: usize,
}

impl<'a, const SHARDS: usize, T> PooledSlice<'a, SHARDS, T> {
    /// Create a new PooledSlice from a pooled Vec and length.
    ///
    /// # Panics
    ///
    /// Panics if `len` exceeds the buffer's length.
    pub fn new(buf: Pooled<'a, SHARDS, Vec<T>>, len: usize) -> Self {
        assert!(
            len <= buf.len(),
            "PooledSlice length {} exceeds buffer length {}",
            len,
            buf.len()
        );
        Self { buf, len }
    }

    /// Get a slice view of the valid data.
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    /// Get a mutable slice view of the valid data.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }

    /// Get the length of valid data.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Consume and return the inner pooled buffer.
    pub fn into_inner(self) -> Pooled<'a, SHARDS, Vec<T>> {
        self.buf
    }
}

impl<'a, const SHARDS: usize, T> AsRef<[T]> for PooledSlice<'a, SHARDS, T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<'a, const SHARDS: usize, T> AsMut<[T]> for PooledSlice<'a, SHARDS, T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

/// Owned wrapper for pooled Vec with tracked length (holds Arc<Pool>).
///
/// Like PooledSlice but owns an Arc to the pool, making it 'static.
pub struct PooledSliceOwned<const SHARDS: usize, T> {
    buf: PooledOwned<SHARDS, Vec<T>>,
    len: usize,
}

impl<const SHARDS: usize, T> PooledSliceOwned<SHARDS, T> {
    /// Create a new PooledSliceOwned from a pooled Vec and length.
    pub fn new(buf: PooledOwned<SHARDS, Vec<T>>, len: usize) -> Self {
        assert!(
            len <= buf.len(),
            "PooledSliceOwned length {} exceeds buffer length {}",
            len,
            buf.len()
        );
        Self { buf, len }
    }

    /// Get a slice view of the valid data.
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    /// Get a mutable slice view of the valid data.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }

    /// Get the length of valid data.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Consume and return the inner pooled buffer.
    pub fn into_inner(self) -> PooledOwned<SHARDS, Vec<T>> {
        self.buf
    }
}

impl<const SHARDS: usize, T> AsRef<[T]> for PooledSliceOwned<SHARDS, T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<const SHARDS: usize, T> AsMut<[T]> for PooledSliceOwned<SHARDS, T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

/// Helper to create Arc-wrapped Pool for shared access.
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
    pub fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        Self(Arc::new(Pool::new(max_buffers, trim_capacity)))
    }

    /// Get a buffer from the shared pool.
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
            let mut shard = self.0.shards[shard_idx].lock();
            shard.try_get()
        };

        if value.is_none() {
            // Try other shards before allocating
            for i in 1..SHARDS {
                let idx = (shard_idx + i) % SHARDS;
                let mut shard = self.0.shards[idx].lock();
                if let Some(v) = shard.try_get() {
                    value = Some(v);
                    break;
                }
            }
        }

        let mut value = value.unwrap_or_default();
        init(&mut value);

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

/// Macro to declare a global buffer pool with lazy initialization.
///
/// Creates a static pool that is initialized on first access using `OnceLock`.
/// The pool is allocated with `Box::leak` to obtain a `'static` reference.
///
/// # Example
///
/// ```
/// use kithara_bufpool::global_pool;
///
/// // Declare a global pool for Vec<f32>
/// global_pool!(f32_pool, F32_POOL, 32, Vec<f32>, 1024, 32768);
///
/// // Access via the function name
/// let pool = f32_pool();
/// let buf = pool.get();
/// ```
///
/// # Generated Items
///
/// - Static: `$static_name` (typically UPPER_CASE)
/// - Function: `$fn_name()` (typically snake_case)
///
/// # Parameters
///
/// - `$fn_name`: Function name (snake_case)
/// - `$static_name`: Static variable name (UPPER_CASE)
/// - `$shards`: Number of shards
/// - `$type`: Type of buffer (e.g., `Vec<u8>`, `Vec<f32>`)
/// - `$max_buffers`: Maximum total buffers across all shards
/// - `$trim_capacity`: Shrink buffers to this capacity when returning to pool
#[macro_export]
macro_rules! global_pool {
    ($fn_name:ident, $static_name:ident, $shards:expr, $type:ty, $max_buffers:expr, $trim_capacity:expr) => {
        static $static_name: std::sync::OnceLock<&'static $crate::Pool<$shards, $type>> =
            std::sync::OnceLock::new();

        #[allow(dead_code)]
        fn $fn_name() -> &'static $crate::Pool<$shards, $type> {
            $static_name.get_or_init(|| {
                let pool = $crate::Pool::<$shards, $type>::new($max_buffers, $trim_capacity);
                Box::leak(Box::new(pool))
            })
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_basic() {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);
        let mut buf = pool.get();
        buf.extend_from_slice(b"hello");
        assert_eq!(&buf[..], b"hello");
    }

    #[test]
    fn test_pool_reuse() {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);

        {
            let mut buf = pool.get();
            buf.extend_from_slice(b"data");
        } // buf returned to pool

        let buf = pool.get();
        assert_eq!(buf.len(), 0); // Should be cleared
        assert!(buf.capacity() > 0); // But capacity retained
    }

    #[test]
    fn test_pool_f32() {
        let pool = Pool::<4, Vec<f32>>::new(16, 1024);
        let mut buf = pool.get_with(|b| b.resize(100, 0.0));
        assert_eq!(buf.len(), 100);
        buf[0] = 1.5;
        assert_eq!(buf[0], 1.5);
    }

    #[test]
    fn test_shared_pool() {
        let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
        let pool2 = pool.clone();

        let mut buf1 = pool.get();
        buf1.push(1);

        let mut buf2 = pool2.get();
        buf2.push(2);

        assert_eq!(buf1[0], 1);
        assert_eq!(buf2[0], 2);
    }

    #[test]
    fn test_pooled_slice() {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);
        let mut buf = pool.get_with(|b| b.resize(100, 0));

        // Simulate reading 50 bytes
        for i in 0..50 {
            buf[i] = i as u8;
        }

        let slice = PooledSlice::new(buf, 50);
        assert_eq!(slice.len(), 50);
        assert_eq!(slice.as_slice().len(), 50);
        assert_eq!(slice.as_slice()[0], 0);
        assert_eq!(slice.as_slice()[49], 49);
    }

    #[test]
    fn test_pooled_slice_as_ref() {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);
        let buf = pool.get_with(|b| {
            b.clear();
            b.extend_from_slice(b"hello world");
        });

        let slice = PooledSlice::new(buf, 5);
        let as_ref: &[u8] = slice.as_ref();
        assert_eq!(as_ref, b"hello");
    }

    #[test]
    #[should_panic(expected = "exceeds buffer length")]
    fn test_pooled_slice_invalid_len() {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);
        let buf = pool.get_with(|b| b.resize(10, 0));
        let _ = PooledSlice::new(buf, 20); // Should panic
    }

    #[test]
    fn test_global_pool_macro() {
        global_pool!(test_pool, TEST_POOL, 4, Vec<u8>, 16, 1024);

        let mut buf1 = test_pool().get();
        buf1.push(42);
        assert_eq!(buf1[0], 42);
        drop(buf1);

        let buf2 = test_pool().get();
        assert_eq!(buf2.len(), 0); // Cleared on return to pool
        assert!(buf2.capacity() > 0); // Capacity retained
    }
}
