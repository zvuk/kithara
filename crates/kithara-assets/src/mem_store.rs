#![forbid(unsafe_code)]

//! In-memory asset store backend.

use std::{
    io::{Error as IoError, ErrorKind},
    path::Path,
    sync::Arc,
};

use kithara_storage::{AvailabilityObserver, MemOptions, MemResource, Resource, StorageResource};
use tokio_util::sync::CancellationToken;

use crate::{
    AssetResourceState,
    base::{Assets, Capabilities},
    error::{AssetsError, AssetsResult},
    index::{AvailabilityIndex, ScopedAvailabilityObserver},
    key::ResourceKey,
};

/// In-memory [`Assets`] implementation — stateless factory.
///
/// Each `open_resource_with_ctx` call creates a fresh [`MemResource`].
/// The [`CachedAssets`](crate::cache::CachedAssets) LRU decorator is the
/// single owner of resource handles; when a handle is evicted from LRU,
/// its `Arc` ref-count drops and memory is freed.
///
/// `MemAssetStore` has the same `Res = StorageResource` as [`DiskAssetStore`](crate::disk_store::DiskAssetStore),
/// allowing both to be used through the same decorator chain.
#[derive(Clone, Debug)]
pub struct MemAssetStore {
    asset_root: String,
    cancel: CancellationToken,
    mem_resource_capacity: Option<usize>,
    availability: AvailabilityIndex,
}

impl MemAssetStore {
    /// Create a new in-memory asset store with its own unshared
    /// [`AvailabilityIndex`].
    pub fn new<S: Into<String>>(
        asset_root: S,
        cancel: CancellationToken,
        mem_resource_capacity: Option<usize>,
    ) -> Self {
        Self::with_availability(
            asset_root,
            cancel,
            mem_resource_capacity,
            AvailabilityIndex::new(),
        )
    }

    /// Like [`MemAssetStore::new`] but shares the given aggregate
    /// availability handle.
    pub(crate) fn with_availability<S: Into<String>>(
        asset_root: S,
        cancel: CancellationToken,
        mem_resource_capacity: Option<usize>,
        availability: AvailabilityIndex,
    ) -> Self {
        Self {
            asset_root: asset_root.into(),
            cancel,
            mem_resource_capacity,
            availability,
        }
    }

    fn scoped_observer(&self, key: &ResourceKey) -> Arc<dyn AvailabilityObserver> {
        ScopedAvailabilityObserver::new(key.clone(), self.availability.clone())
    }
}

impl Assets for MemAssetStore {
    type Res = StorageResource;
    type Context = ();
    type IndexRes = MemResource;

    fn capabilities(&self) -> Capabilities {
        Capabilities::CACHE | Capabilities::PROCESSING
    }

    fn root_dir(&self) -> &Path {
        Path::new("")
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
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

        Err(IoError::new(ErrorKind::NotFound, "resource missing").into())
    }

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
        Ok(StorageResource::Mem(mem))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        if let ResourceKey::Relative(rel) = key
            && rel.is_empty()
        {
            return Err(AssetsError::InvalidKey);
        }

        Ok(AssetResourceState::Missing)
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_storage::ResourceExt;
    use kithara_test_utils::kithara;

    use super::*;

    fn make_mem_store() -> MemAssetStore {
        MemAssetStore::new("test_asset", CancellationToken::new(), None)
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
