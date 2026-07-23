//! Sharded buffer pool — split into one module per public type.

mod core;
mod owned;
mod pooled;
mod reuse;
mod shard;
mod shared;

pub use core::{Pool, PoolStats};

pub use owned::PooledOwned;
pub use pooled::Pooled;
pub use reuse::Reuse;
pub use shared::SharedPool;
