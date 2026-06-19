#![forbid(unsafe_code)]

//! Persistent disk assets store for Kithara.
//!
//! The public contract is the unified [`AssetStore`] type. See the crate
//! `CONTEXT.md` for key mapping, lease/pin semantics, and the global index.

mod acquisition;
mod base;
pub(crate) mod cache;
mod deleter;
mod disk_store;
mod error;
mod evict;
mod flush;
mod identity;
pub mod index;
mod key;
mod lease;
mod mem_store;
mod naming;
mod process;
mod resource;
mod scope;
mod state;
mod store;
mod unified;

pub use acquisition::{AcquisitionResult, RawWriteHandle, ReadSide, WriteSide};
pub use base::Assets;
pub use cache::{CachedAssets, CachedReader, CachedWriter};
#[cfg(not(target_arch = "wasm32"))]
pub use disk_store::DiskAssetStore;
pub use error::{AssetsError, AssetsResult};
pub use evict::EvictAssets;
pub use flush::{FlushHub, FlushPolicy};
pub use identity::RequestIdentity;
pub use index::{DemandLease, EvictConfig, ProducerHandle};
pub use key::{ResourceKey, asset_root_for_url};
#[doc(hidden)]
pub use kithara_bufpool::BytePool;
pub use lease::{LeaseAssets, LeaseGuard, LeaseReader, LeaseWriter};
pub use mem_store::MemAssetStore;
pub use naming::{
    AssetScopeDelegate, DefaultAssetScopeDelegate, safe_path_component, url_fingerprint,
};
pub use process::{ProcessChunkFn, ProcessedReader, ProcessedWriter, ProcessingAssets};
pub use resource::{BaseReader, BaseWriter};
pub use scope::AssetScope;
pub use state::AssetResourceState;
pub use store::{
    AssetReader, AssetResource, AssetStoreBuilder, AssetWriter, OnInvalidatedFn, StoreOptions,
};
pub use unified::AssetStore;
