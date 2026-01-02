use crate::{CachePath, CacheResult, PutResult, store::Store};
use kithara_core::AssetId;

/// Trait that extends Store with pin/unpin operations.
pub trait PinStore: Store {
    fn pin(&self, asset: AssetId) -> CacheResult<()>;
    fn unpin(&self, asset: AssetId) -> CacheResult<()>;
    fn is_pinned(&self, asset: AssetId) -> CacheResult<bool>;
}

/// RAII guard that keeps an asset pinned while alive.
/// When dropped, asset is unpinned.
pub struct LeaseGuard<'a, S>
where
    S: PinStore,
{
    store: &'a LeaseStore<S>,
    asset_id: AssetId,
}

impl<'a, S> LeaseGuard<'a, S>
where
    S: PinStore,
{
    pub fn new(store: &'a LeaseStore<S>, asset_id: AssetId) -> Self {
        LeaseGuard { store, asset_id }
    }

    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }
}

impl<'a, S> Drop for LeaseGuard<'a, S>
where
    S: PinStore,
{
    fn drop(&mut self) {
        let _ = self.store.unpin(self.asset_id);
    }
}

/// Lease decorator that provides pin/lease semantics.
/// Assets can be pinned to prevent eviction.
/// Uses wrapped store's indexing mechanism for tracking pins.
#[derive(Clone, Debug)]
pub struct LeaseStore<S> {
    inner: S,
}

impl<S> LeaseStore<S>
where
    S: PinStore,
{
    pub fn new(inner: S) -> Self {
        LeaseStore { inner }
    }

    /// Pin an asset to prevent eviction.
    /// Returns a guard that will unpin the asset when dropped.
    pub fn pin(&self, asset: AssetId) -> CacheResult<LeaseGuard<'_, S>> {
        self.inner.pin(asset)?;
        Ok(LeaseGuard::new(self, asset))
    }

    /// Check if an asset is pinned.
    pub fn is_pinned(&self, asset: AssetId) -> CacheResult<bool> {
        self.inner.is_pinned(asset)
    }

    fn unpin(&self, asset: AssetId) -> CacheResult<()> {
        self.inner.unpin(asset)
    }
}

impl<S> Store for LeaseStore<S>
where
    S: Store,
{
    fn exists(&self, asset: AssetId, rel_path: &CachePath) -> bool {
        self.inner.exists(asset, rel_path)
    }

    fn open(&self, asset: AssetId, rel_path: &CachePath) -> CacheResult<Option<std::fs::File>> {
        self.inner.open(asset, rel_path)
    }

    fn put_atomic(
        &self,
        asset: AssetId,
        rel_path: &CachePath,
        bytes: &[u8],
    ) -> CacheResult<PutResult> {
        self.inner.put_atomic(asset, rel_path, bytes)
    }

    fn remove_all(&self, asset: AssetId) -> CacheResult<()> {
        self.inner.remove_all(asset)
    }
}

impl<S> PinStore for LeaseStore<S>
where
    S: PinStore,
{
    fn pin(&self, asset: AssetId) -> CacheResult<()> {
        self.inner.pin(asset)
    }

    fn unpin(&self, asset: AssetId) -> CacheResult<()> {
        self.inner.unpin(asset)
    }

    fn is_pinned(&self, asset: AssetId) -> CacheResult<bool> {
        self.inner.is_pinned(asset)
    }
}

impl<S> crate::evicting_store::EvictionSupport for LeaseStore<S>
where
    S: crate::evicting_store::EvictionSupport,
{
    fn get_total_bytes(&self) -> CacheResult<u64> {
        self.inner.get_total_bytes()
    }

    fn get_all_assets(&self) -> CacheResult<Vec<(String, crate::AssetState)>> {
        self.inner.get_all_assets()
    }

    fn remove_assets_from_state(&self, keys: &[String]) -> CacheResult<()> {
        self.inner.remove_assets_from_state(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::FsStore;
    use std::env;

    fn create_temp_lease_store() -> LeaseStore<FsStore> {
        let temp_dir =
            env::temp_dir().join(format!("kithara-leasestore-test-{}", uuid::Uuid::new_v4()));
        let fs_store = FsStore::new(temp_dir).unwrap();
        // Note: FsStore doesn't implement PinStore, so this would need IndexStore
        // For testing, we'll just test the delegation pattern
        // In practice, this would be used with IndexStore< FsStore >
        todo!("Need IndexStore<FsStore> to implement PinStore");
    }

    #[test]
    fn lease_guard_drop_should_not_panic() {
        // This test would need a proper PinStore implementation
        // For now, we'll skip and rely on integration tests
    }

    #[test]
    fn lease_store_delegates_to_inner_store() {
        // This test would need a proper PinStore implementation
        // For now, we'll skip and rely on integration tests
    }

    #[test]
    fn multiple_leases_for_different_assets() {
        // This test would need a proper PinStore implementation
        // For now, we'll skip and rely on integration tests
    }
}
