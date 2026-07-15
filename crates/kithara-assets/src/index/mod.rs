#![forbid(unsafe_code)]

mod availability;
mod demand;
mod lru;
#[cfg(not(target_arch = "wasm32"))]
mod persist;
mod pin;
pub mod schema;
mod transaction;

pub(crate) use availability::{AvailabilityIndex, ScopedAvailabilityObserver};
pub(crate) use demand::{DemandEntry, DemandIndex};
pub use demand::{DemandLease, ProducerHandle};
pub use lru::EvictConfig;
pub(crate) use lru::LruIndex;
pub use pin::PinsIndex;
pub(crate) use transaction::ResourceTransactionIndex;
