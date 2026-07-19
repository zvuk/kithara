#![forbid(unsafe_code)]

mod availability;
mod demand;
mod lru;
pub(crate) mod persistence;
mod pins;
mod transaction;

pub(crate) use availability::{AvailabilityIndex, ScopedAvailabilityObserver};
pub(crate) use demand::{DemandEntry, DemandIndex};
pub use demand::{DemandLease, ProducerHandle};
pub(crate) use lru::{EvictConfig, LruIndex};
pub use persistence::schema;
pub(crate) use persistence::{FlushHub, FlushPolicy};
pub use pins::PinsIndex;
pub(crate) use transaction::ResourceTransactionIndex;
