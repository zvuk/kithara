//! Typed sharded buffer pools with one shared hard byte budget.

#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod __private;
mod budget;
mod buffer;
mod config;
mod error;
mod key;
mod pool;
mod region;
mod ring;
mod schema;
#[cfg(feature = "test-utils")]
pub mod testing;

pub use budget::{OverallBudget, Percent};
pub use buffer::{ByteBuffer, PooledString, PooledVec, SampleBuffer};
pub use config::{PoolConfig, PoolConfigPatch};
pub use error::PoolError;
pub use key::{PoolAlias, PoolKey, PoolKeyWithLen, StringKey, VecKey};
pub use pool::PoolStats;
pub use region::{PoolRegion, RegionStats};
pub use ring::BufferRing;
pub use schema::HasPool;
