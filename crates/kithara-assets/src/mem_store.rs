#![forbid(unsafe_code)]

//! In-memory asset store for ephemeral (non-cacheable) content.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use kithara_platform::RwLock;
use kithara_storage::{MemOptions, MemResource, Resource, StorageResource};
use tokio_util::sync::CancellationToken;

use crate::{
    base::Assets,
    error::{AssetsError, AssetsResult},
    key::ResourceKey,
};

/// In-memory [`Assets`] implementation for ephemeral content.
///
/// All resources are stored in a [`HashMap`] behind a [`RwLock`] keyed by
/// [`ResourceKey`]. Nothing is persisted to disk. Index resources (pins, LRU,
/// coverage) are backed by [`MemResource`] and are not persisted either.
///
/// Uses `kithara_platform::RwLock` which is WASM-safe (spin-loop instead of
/// `Atomics.wait` on the browser main thread).
///
/// `MemAssetStore` has the same `Res = StorageResource` as [`DiskAssetStore`](crate::disk_store::DiskAssetStore),
/// allowing both to be used through the same decorator chain.
#[derive(Clone, Debug)]
pub struct MemAssetStore {
    asset_root: String,
    cancel: CancellationToken,
    mem_resource_capacity: Option<usize>,
    resources: Arc<RwLock<HashMap<ResourceKey, MemResource>>>,
    root_dir: PathBuf,
}

impl MemAssetStore {
    /// Create a new in-memory asset store.
    pub fn new<S: Into<String>>(
        asset_root: S,
        cancel: CancellationToken,
        mem_resource_capacity: Option<usize>,
        root_dir: PathBuf,
    ) -> Self {
        Self {
            asset_root: asset_root.into(),
            cancel,
            mem_resource_capacity,
            resources: Arc::new(RwLock::new(HashMap::new())),
            root_dir,
        }
    }
}

impl Assets for MemAssetStore {
    type Res = StorageResource;
    type Context = ();
    type IndexRes = MemResource;

    fn supports_cache(&self) -> bool {
        true
    }

    fn supports_evict(&self) -> bool {
        false
    }

    fn supports_lease(&self) -> bool {
        false
    }

    fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        // Return existing resource if present.
        if let Some(entry) = self.resources.read().get(key) {
            return Ok(StorageResource::Mem(entry.clone()));
        }

        // Validate relative keys.
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
        let mem = Resource::open(self.cancel.clone(), options).map_err(AssetsError::Storage)?;
        self.resources.write().insert(key.clone(), mem.clone());
        Ok(StorageResource::Mem(mem))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn open_coverage_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        Ok(MemResource::new(self.cancel.clone()))
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        self.resources.write().clear();
        Ok(())
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.resources.write().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_storage::ResourceExt;
    use kithara_test_utils::kithara;

    use super::*;

    fn make_mem_store() -> MemAssetStore {
        MemAssetStore::new(
            "test_asset",
            CancellationToken::new(),
            None,
            PathBuf::from("/tmp"),
        )
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_creates_mem_resource() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.open_resource(&key).unwrap();
        assert!(matches!(res, StorageResource::Mem(_)));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_returns_same_resource() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res1 = store.open_resource(&key).unwrap();
        res1.write_at(0, b"data").unwrap();

        let res2 = store.open_resource(&key).unwrap();
        let mut buf = [0u8; 4];
        let n = res2.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn write_commit_read_roundtrip() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.open_resource(&key).unwrap();
        res.write_at(0, b"segment data").unwrap();
        res.commit(Some(12)).unwrap();

        let mut buf = [0u8; 12];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"segment data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn remove_resource_then_open_creates_new() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.open_resource(&key).unwrap();
        res.write_at(0, b"old data").unwrap();
        res.commit(Some(8)).unwrap();

        store.remove_resource(&key).unwrap();

        let res2 = store.open_resource(&key).unwrap();
        // New resource should have no data
        assert_eq!(res2.len(), None);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn delete_asset_clears_all() {
        let store = make_mem_store();

        for i in 0..3 {
            let key = ResourceKey::new(format!("seg_{i}.m4s"));
            let res = store.open_resource(&key).unwrap();
            res.write_at(0, b"data").unwrap();
        }

        assert_eq!(store.resources.read().len(), 3);
        store.delete_asset().unwrap();
        assert_eq!(store.resources.read().len(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn no_path_for_mem_resources() {
        let store = make_mem_store();
        let key = ResourceKey::new("seg_0.m4s");

        let res = store.open_resource(&key).unwrap();
        assert!(res.path().is_none());
    }

    #[kithara::test]
    fn mem_store_reports_unsupported_decorators() {
        let store = make_mem_store();
        assert!(!store.supports_evict());
        assert!(!store.supports_lease());
    }
}
