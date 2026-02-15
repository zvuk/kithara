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
//! use kithara_bufpool::pcm_pool;
//!
//! // Get buffer from the global PCM pool
//! let mut buf = pcm_pool().get();
//! buf.resize(1024, 0.0);
//! // Use buf...
//! // Automatically returned to pool on drop
//! ```

#![forbid(unsafe_code)]

mod global;
mod pool;

pub use global::{BytePool, PcmBuf, PcmPool, byte_pool, pcm_pool};
// Low-level pool internals (used by type aliases above; prefer BytePool/PcmPool/PcmBuf)
#[doc(hidden)]
pub use pool::{Pool, Pooled, PooledOwned, Reuse, SharedPool};

#[cfg(test)]
mod tests;
