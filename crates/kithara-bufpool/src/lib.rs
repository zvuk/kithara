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
mod schema;

pub use budget::OverallBudget;
pub use buffer::{ByteBuffer, PooledString, PooledVec, SampleBuffer};
pub use config::{PoolConfig, PoolConfigPatch};
pub use error::PoolError;
pub use key::{PoolAlias, PoolKey, PoolKeyWithLen, StringKey, VecKey};
pub use kithara_derive::Pool;
pub use kithara_ring::{RingCons, RingHalves, RingProd};
pub use pool::PoolStats;
pub use region::{PoolRegion, PoolSlot, RegionStats};
pub use schema::HasPool;
