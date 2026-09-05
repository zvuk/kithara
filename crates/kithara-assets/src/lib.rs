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
#[doc(hidden)]
pub use index::pending_resource::{
    PendingResourceCleanupError, ResourceAttachment, ResourceLease, WriterEpoch, WriterHandle,
    WriterOutcome,
};
pub use index::persistence::{FlushHub, FlushPolicy, FlushPolicyPatch};
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
pub use layout::{
    AssetLayout, AssetLayoutRegistry, AssetResource, AssetScope, AssetSource, DefaultLayout,
    ResourceKey,
};
pub use resource::{
    AcquisitionResult, AssetResourceState, BaseReader, BaseWriter, RawWriteHandle, ReadSide,
    RequestIdentity, WriteSide,
};
pub use store::{
    AssetReader, AssetStore, AssetStoreBuilder, AssetStoreConfig, AssetStoreConfigPatch,
    AssetWriter, ResourceAcquisition, StorageBackend,
};
