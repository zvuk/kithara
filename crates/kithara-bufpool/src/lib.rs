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

pub use global::{BytePool, PcmBuf, PcmPool, byte_pool, pcm_pool};
pub use pool::{Pool, Pooled, PooledOwned, Reuse, SharedPool};
pub use slice::{PooledSlice, PooledSliceOwned};

#[cfg(test)]
mod tests;
