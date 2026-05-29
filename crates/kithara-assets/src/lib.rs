#![forbid(unsafe_code)]

//! Persistent disk assets store for Kithara.
//!
//! The public contract is the unified [`AssetStore`] type. See the crate
//! `README.md` for key mapping, lease/pin semantics, and the global index.

mod base;
pub(crate) mod cache;
mod deleter;
mod disk_store;
mod error;
mod evict;
mod flush;
pub mod index;
mod key;
mod lease;
mod mem_store;
mod process;
mod state;
mod store;
mod unified;

pub use base::{Assets, ResourceHandle};
pub use cache::{CachedAssets, CachedResource};
#[cfg(not(target_arch = "wasm32"))]
pub use disk_store::DiskAssetStore;
pub use error::{AssetsError, AssetsResult};
pub use evict::EvictAssets;
pub use flush::{FlushHub, FlushPolicy};
pub use index::EvictConfig;
pub use key::{ResourceKey, asset_root_for_url};
#[doc(hidden)]
pub use kithara_bufpool::BytePool;
pub use lease::{LeaseAssets, LeaseGuard, LeaseResource};
pub use mem_store::MemAssetStore;
pub use process::{ProcessChunkFn, ProcessedResource, ProcessingAssets};
pub use state::AssetResourceState;
pub use store::{AssetResource, AssetStoreBuilder, OnInvalidatedFn, StoreOptions};
pub use unified::AssetStore;
