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

mod global;
mod pool;
mod slice;

pub use global::{BytePool, byte_pool};
pub use pool::{Pool, Pooled, PooledOwned, Reuse, SharedPool};
pub use slice::{PooledSlice, PooledSliceOwned};

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
