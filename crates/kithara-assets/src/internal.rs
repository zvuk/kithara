#![forbid(unsafe_code)]

use std::collections::HashSet;

pub use kithara_bufpool::{BytePool, byte_pool};
pub use kithara_coverage::DiskCoverage;
use kithara_storage::ResourceExt;

#[cfg(target_arch = "wasm32")]
pub use crate::base::Assets;
use crate::error::AssetsResult;
#[cfg(not(target_arch = "wasm32"))]
pub use crate::{base::Assets, disk_store::DiskAssetStore, store::DiskStore};
pub use crate::{
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
    mem_store::MemAssetStore,
    process::{ProcessedResource, ProcessingAssets},
};

/// Test-only handle for interacting with persisted pins index.
pub struct PinsIndex<R: ResourceExt>(crate::index::PinsIndex<R>);

impl<R: ResourceExt> PinsIndex<R> {
    /// Open `pins.bin` through the given base assets store.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the underlying index resource cannot be opened.
    pub fn open<A: Assets<IndexRes = R>>(assets: &A, pool: BytePool) -> AssetsResult<Self> {
        crate::index::PinsIndex::open(assets, pool).map(Self)
    }

    /// Load current pinned-asset set.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if reading from storage fails.
    pub fn load(&self) -> AssetsResult<HashSet<String>> {
        self.0.load()
    }

    /// Persist pinned-asset set.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if writing to storage fails.
    pub fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        self.0.store(pins)
    }
}
