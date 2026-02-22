#![forbid(unsafe_code)]

pub use kithara_bufpool::{BytePool, byte_pool};
pub use kithara_coverage::DiskCoverage;

#[cfg(target_arch = "wasm32")]
pub use crate::base::Assets;
#[cfg(not(target_arch = "wasm32"))]
pub use crate::base::{Assets, DiskAssetStore};
pub use crate::{
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
    mem_store::MemAssetStore,
    process::{ProcessedResource, ProcessingAssets},
};
