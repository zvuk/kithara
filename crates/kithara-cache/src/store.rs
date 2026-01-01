use crate::{CachePath, CacheResult, PutResult};
use kithara_core::AssetId;

/// Minimal storage contract for blob operations.
/// This is the foundational interface that all cache layers must implement.
pub trait Store {
    /// Check if a resource exists for the given asset and path.
    fn exists(&self, asset: AssetId, rel_path: &CachePath) -> bool;

    /// Open a resource for reading, if it exists.
    fn open(&self, asset: AssetId, rel_path: &CachePath) -> CacheResult<Option<std::fs::File>>;

    /// Atomically write data to a resource.
    /// Returns the number of bytes written.
    fn put_atomic(
        &self,
        asset: AssetId,
        rel_path: &CachePath,
        bytes: &[u8],
    ) -> CacheResult<PutResult>;

    /// Remove all resources for an asset.
    fn remove_all(&self, asset: AssetId) -> CacheResult<()>;
}
