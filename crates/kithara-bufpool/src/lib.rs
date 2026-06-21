//! Generic sharded buffer pool for zero-allocation hot paths.
//!
//! Thread-safe, RAII-guarded, sharded buffers for any type implementing
//! the `Reuse` trait (`Vec<u8>`, `Vec<f32>`, etc.). See the crate
//! `README.md` for usage and `CONTEXT.md` for the allocation flow.

#![forbid(unsafe_code)]

mod global;
mod growth;
mod pool;

pub use global::{BytePool, PcmBuf, PcmPool};
pub use growth::BudgetExhausted;
pub use pool::ByteBudget;
#[doc(hidden)]
pub use pool::{Pool, PoolStats, Pooled, PooledOwned, Reuse, SharedPool};
