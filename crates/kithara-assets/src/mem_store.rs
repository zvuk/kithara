#![forbid(unsafe_code)]

//! In-memory asset store backend.

use std::{
    io::{Error as IoError, ErrorKind},
    path::Path,
    sync::{Arc, Weak},
};

use dashmap::DashMap;
use kithara_storage::{AvailabilityObserver, MemOptions, MemResource, Resource, StorageResource};
use tokio_util::sync::CancellationToken;

use crate::{
    AssetResourceState,
    base::{Assets, Capabilities},
    deleter::AssetDeleter,
    error::{AssetsError, AssetsResult},
    index::{AvailabilityIndex, ScopedAvailabilityObserver},
    key::ResourceKey,
};

/// In-memory [`Assets`] implementation.
///
/// Shares existing [`MemResource`] instances for the same key via an internal
/// weak cache. This ensures that multiple handles to the same resource share
/// the same underlying data and status, even if the primary handle is
/// evicted from [`CachedAssets`](crate::cache::CachedAssets).
#[derive(Clone, Debug)]
pub struct MemAssetStore {
    /// Weak cache of active resources to ensure sharing.
    active_resources: Arc<DashMap<String, Weak<MemResource>>>,
    /// Single canonical removal channel. Synchronises in-memory
    /// `active_resources` clearing with the [`AvailabilityIndex`].
    /// See [`crate::deleter`].
    deleter: Arc<dyn AssetDeleter>,
    availability: AvailabilityIndex,
    cancel: CancellationToken,
    mem_resource_capacity: Option<usize>,
    asset_root: String,
}

/// Mem-backed [`AssetDeleter`].
///
/// Scoped to a single `own_asset_root` because mem backends are
/// single-asset by construction. Foreign-asset deletion only touches
/// the shared in-memory indexes — there are no per-resource handles
/// for a foreign `asset_root` in this backend instance.
#[derive(Debug)]
pub(crate) struct MemAssetDeleter {
    active_resources: Arc<DashMap<String, Weak<MemResource>>>,
    availability: AvailabilityIndex,
    lru: crate::index::LruIndex,
    pins: crate::index::PinsIndex,
    own_asset_root: String,
}

impl MemAssetDeleter {
    pub(crate) fn new(
        own_asset_root: String,
        availability: AvailabilityIndex,
        pins: crate::index::PinsIndex,
        lru: crate::index::LruIndex,
        active_resources: Arc<DashMap<String, Weak<MemResource>>>,
    ) -> Self {
        Self {
            active_resources,
            availability,
            lru,
            pins,
            own_asset_root,
        }
    }
}

impl AssetDeleter for MemAssetDeleter {
    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        if asset_root == self.own_asset_root {
            self.active_resources.clear();
        }
        self.availability.clear_root(asset_root);
        // Best-effort cleanup of every index — first error wins
        // (same contract as `DiskAssetDeleter::delete_asset`).
        let pins_result = self.pins.remove(asset_root).map(|_| ());
        let lru_result = self.lru.remove(asset_root);
        pins_result.and(lru_result)
    }

    fn remove_resource(&self, asset_root: &str, key: &ResourceKey) -> AssetsResult<()> {
        if asset_root == self.own_asset_root {
            self.active_resources
                .remove(&MemAssetStore::resource_cache_key(key));
        }
        self.availability.remove(asset_root, key);
        Ok(())
    }
}

impl MemAssetStore {
    /// Create a new in-memory asset store with its own unshared
    /// [`AvailabilityIndex`].
    pub fn new<S: Into<String>>(
        asset_root: S,
        cancel: CancellationToken,
        mem_resource_capacity: Option<usize>,
        pool: &kithara_bufpool::BytePool,
    ) -> Self {
        Self::with_availability(
            asset_root,
            cancel,
            mem_resource_capacity,
            AvailabilityIndex::new(),
            pool,
        )
    }

    pub(crate) fn resource_cache_key(key: &ResourceKey) -> String {
        match key {
            ResourceKey::Relative(path) => path.clone(),
            ResourceKey::Absolute(path) => path.to_string_lossy().to_string(),
        }
    }

    fn scoped_observer(&self, key: &ResourceKey) -> Arc<dyn AvailabilityObserver> {
        ScopedAvailabilityObserver::new(
            self.asset_root.clone(),
            key.clone(),
            self.availability.clone(),
        )
    }

    /// Like [`MemAssetStore::new`] but shares the given aggregate
    /// availability handle.
    pub(crate) fn with_availability<S: Into<String>>(
        asset_root: S,
        cancel: CancellationToken,
        mem_resource_capacity: Option<usize>,
        availability: AvailabilityIndex,
        pool: &kithara_bufpool::BytePool,
    ) -> Self {
        let asset_root = asset_root.into();
        let active_resources = Arc::new(DashMap::new());
        let _ = pool;
        // Standalone construction: ephemeral indexes (no on-disk
        // backing). Production builder uses shared instances.
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
        let deleter: Arc<dyn AssetDeleter> = Arc::new(MemAssetDeleter::new(
            asset_root.clone(),
            availability.clone(),
            pins,
            lru,
            Arc::clone(&active_resources),
        ));
        Self::with_availability_and_deleter(
            asset_root,
            cancel,
            mem_resource_capacity,
            availability,
            active_resources,
            deleter,
        )
    }

    /// Like [`Self::with_availability`] but accepts a pre-built
    /// [`AssetDeleter`] so the production builder can share the same
    /// deleter instance with the LRU evictor (`EvictAssets`).
    pub(crate) fn with_availability_and_deleter<S: Into<String>>(
        asset_root: S,
        cancel: CancellationToken,
        mem_resource_capacity: Option<usize>,
        availability: AvailabilityIndex,
        active_resources: Arc<DashMap<String, Weak<MemResource>>>,
        deleter: Arc<dyn AssetDeleter>,
    ) -> Self {
        Self {
            cancel,
            mem_resource_capacity,
            availability,
            active_resources,
            deleter,
            asset_root: asset_root.into(),
        }
    }
}

impl Assets for MemAssetStore {
    type Context = ();
    type IndexRes = MemResource;
    type Res = StorageResource;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if let ResourceKey::Relative(rel) = key
            && rel.is_empty()
        {
            return Err(AssetsError::InvalidKey);
        }

        let cache_key = Self::resource_cache_key(key);
        if let Some(weak) = self.active_resources.get(&cache_key)
            && let Some(res) = weak.upgrade()
        {
            return Ok(StorageResource::from((*res).clone()));
        }

        let mut options = MemOptions::default();
        if let Some(capacity) = self.mem_resource_capacity
            && capacity > 0
        {
            options.capacity = capacity;
        }
        let mem = Resource::open_with_observer(
            self.cancel.clone(),
            options,
            Some(self.scoped_observer(key)),
        )
        .map_err(AssetsError::Storage)?;

        let shared = Arc::new(mem);
        self.active_resources
            .insert(cache_key, Arc::downgrade(&shared));

        Ok(StorageResource::from((*shared).clone()))
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::CACHE | Capabilities::PROCESSING
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        // Single canonical removal channel — see [`crate::deleter`].
        self.deleter.delete_asset(&self.asset_root)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if let ResourceKey::Relative(rel) = key
            && rel.is_empty()
        {
            return Err(AssetsError::InvalidKey);
        }

        let cache_key = Self::resource_cache_key(key);
        if let Some(weak) = self.active_resources.get(&cache_key)
            && let Some(res) = weak.upgrade()
        {
            return Ok(StorageResource::from((*res).clone()));
        }

        Err(IoError::new(ErrorKind::NotFound, "resource missing").into())
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.deleter.remove_resource(&self.asset_root, key)
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        if let ResourceKey::Relative(rel) = key
            && rel.is_empty()
        {
            return Err(AssetsError::InvalidKey);
        }

        Ok(AssetResourceState::Missing)
    }

    fn root_dir(&self) -> &Path {
        Path::new("")
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_storage::ResourceExt;
    use kithara_test_utils::kithara;

    use super::*;

    fn make_mem_store() -> MemAssetStore {
        MemAssetStore::new(
            "test_asset",
            CancellationToken::new(),
            None,
            &crate::BytePool::default(),
        )
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn acquire_creates_mem_resource() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.acquire_resource(&key).unwrap();
        assert!(matches!(res, StorageResource::Mem(_)));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn write_commit_read_roundtrip() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"segment data").unwrap();
        res.commit(Some(12)).unwrap();

        let mut buf = [0u8; 12];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"segment data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn no_path_for_mem_resources() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.acquire_resource(&key).unwrap();
        assert!(res.path().is_none());
    }

    #[kithara::test]
    fn mem_store_capabilities() {
        let store = make_mem_store();
        let caps = store.capabilities();
        assert!(caps.contains(Capabilities::CACHE));
        assert!(caps.contains(Capabilities::PROCESSING));
        assert!(!caps.contains(Capabilities::EVICT));
        assert!(!caps.contains(Capabilities::LEASE));
    }
}
