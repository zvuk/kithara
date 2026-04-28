#![forbid(unsafe_code)]

use std::collections::HashSet;

pub use kithara_bufpool::{BytePool, byte_pool};
use tokio_util::sync::CancellationToken;

#[cfg(target_arch = "wasm32")]
pub use crate::base::Assets;
use crate::error::AssetsResult;
#[cfg(not(target_arch = "wasm32"))]
pub use crate::{base::Assets, disk_store::DiskAssetStore};
pub use crate::{
    cache::CachedAssets,
    evict::EvictAssets,
    index::{EvictConfig, FlushHub, FlushPolicy, schema},
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
    mem_store::MemAssetStore,
    process::{ProcessedResource, ProcessingAssets},
};

/// Test-only handle for interacting with persisted pins index.
///
/// The internal [`crate::index::PinsIndex`] is now an
/// [`Arc`]-shared in-memory + disk-backed handle. This wrapper keeps
/// the historical `internal::PinsIndex::open(assets, pool)` →
/// `load() / store()` API for downstream tests. Each `open` call
/// constructs a fresh disk-backed handle bound to
/// `<root_dir>/_index/pins.bin`.
pub struct PinsIndex {
    inner: crate::index::PinsIndex,
}

impl PinsIndex {
    /// Open `pins.bin` through the given base assets store. Hydrates
    /// initial state from disk; mutations through `store` flush
    /// immediately.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the underlying index resource cannot be opened.
    pub fn open<A: Assets>(assets: &A, pool: &BytePool) -> AssetsResult<Self> {
        let path = assets.root_dir().join("_index").join("pins.bin");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Ok(Self {
            inner: crate::index::PinsIndex::with_persist_at(path, CancellationToken::new(), pool),
        })
    }

    /// Load current pinned-asset set.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if reading from storage fails.
    pub fn load(&self) -> AssetsResult<HashSet<String>> {
        Ok(self.inner.snapshot())
    }

    /// Persist pinned-asset set, replacing any existing state.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if writing to storage fails.
    pub fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let current = self.inner.snapshot();
        for outdated in current.difference(pins) {
            self.inner.remove(outdated)?;
        }
        for added in pins.difference(&current) {
            self.inner.add(added)?;
        }
        self.inner.flush()
    }
}
