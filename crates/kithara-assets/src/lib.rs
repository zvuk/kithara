#![forbid(unsafe_code)]

//! Persistent disk assets store for Kithara.
//!
//! The public contract is the unified [`AssetStore`] type.

mod backend;
mod decorator;
mod error;
mod event;
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
pub use event::{AssetEvent, EvictReason};
use humantime_serde as _;
#[doc(hidden)]
pub use index::pending_resource::{
    PendingResourceCleanupError, ResourceAttachment, ResourceLease, WriterEpoch, WriterHandle,
    WriterOutcome,
};
pub use index::persistence::{FlushHub, FlushPolicy, FlushPolicyPatch};
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
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
mod consts;
