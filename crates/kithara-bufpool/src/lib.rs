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
//! use kithara_bufpool::PcmPool;
//!
//! // Build (or fetch the singleton) at the top of the app and pass it down
//! // through your config structs. Library code should not call
//! // `PcmPool::default()` directly — read the pool from injected config.
//! let pool = PcmPool::default();
//! let mut buf = pool.get();
//! buf.resize(1024, 0.0);
//! // Use buf...
//! // Automatically returned to the pool on drop.
//! ```

#![forbid(unsafe_code)]

mod global;
mod growth;
mod pool;

#[cfg(feature = "internal")]
pub mod internal;

pub use global::{BytePool, PcmBuf, PcmPool};
pub use growth::BudgetExhausted;
// Low-level pool internals (used by type aliases above; prefer BytePool/PcmPool/PcmBuf)
#[doc(hidden)]
pub use pool::{Pool, PoolStats, Pooled, PooledOwned, Reuse, SharedPool};
