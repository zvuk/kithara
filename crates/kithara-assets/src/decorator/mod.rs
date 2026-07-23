mod cache;
mod contract;
mod eviction;
mod lease;
mod processing;

pub use cache::{CachedAssets, CachedReader, CachedWriter};
pub use contract::Assets;
pub(crate) use contract::Capabilities;
pub(crate) use eviction::{ByteRecorder, EvictDeps, EvictionEvents, EvictionRouter};
pub use eviction::{EvictAssets, EvictionSubscription};
pub(crate) use lease::LeaseEvents;
pub use lease::{LeaseAssets, LeaseGuard, LeaseReader, LeaseWriter};
pub use processing::{
    ChunkSink, ProcessCtx, ProcessedReader, ProcessedWriter, ProcessingAssets, ResourceProcessor,
};
