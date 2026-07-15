#![forbid(unsafe_code)]

//! Persistent disk assets store for Kithara.
//!
//! The public contract is the unified [`AssetStore`] type. See the crate
//! `CONTEXT.md` for key mapping, lease/pin semantics, and the global index.

mod backend;
mod decorator;
mod error;
pub mod index;
mod layout;
mod resource;
mod store;

#[cfg(not(target_arch = "wasm32"))]
pub use backend::DiskAssetStore;
pub use backend::MemAssetStore;
pub use decorator::{
    Assets, CachedAssets, CachedReader, CachedWriter, ChunkSink, EvictAssets, EvictionSubscription,
    LeaseAssets, LeaseGuard, LeaseReader, LeaseWriter, ProcessCtx, ProcessedReader,
    ProcessedWriter, ProcessingAssets, ResourceProcessor,
};
pub use error::{AssetsError, AssetsResult};
pub use index::{
    DemandLease, ProducerHandle,
    persistence::{FlushHub, FlushPolicy},
};
#[doc(hidden)]
pub use kithara_bufpool::BytePool;
pub use layout::{
    AssetLayout, AssetLayoutRegistry, AssetResource, AssetScope, AssetSource, DefaultLayout,
    ResourceKey,
};
pub use resource::{
    AcquisitionResult, AssetResourceState, BaseReader, BaseWriter, RawWriteHandle, ReadSide,
    RequestIdentity, WriteSide,
};
pub use store::{
    AssetReader, AssetStore, AssetStoreBuilder, AssetWriter, ResourceAcquisition, StorageBackend,
    StoreOptions,
};
