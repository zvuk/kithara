#![forbid(unsafe_code)]

use std::{
    io::{Error as IoError, ErrorKind},
    path::Path,
    sync::Weak,
};

use dashmap::DashMap;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::{
    AvailabilityObserver, MemOptions, MemResource, Resource, ResourceStatus, StorageResource,
};

use crate::{
    AssetResourceState,
    acquisition::AcquisitionResult,
    base::{Assets, BaseReader, BaseWriter, Capabilities},
    deleter::AssetDeleter,
    error::{AssetsError, AssetsResult},
    identity::RequestIdentity,
    index::{AvailabilityIndex, ScopedAvailabilityObserver},
    key::ResourceKey,
};

/// Composite cache key for mem-backed active resources.
///
/// Identity is part of the key so distinct request identities under the
/// same resource key yield distinct inflight handles. The `ResourceKey`
/// already carries the asset namespace. See the inflight sharing
/// contract in `CONTEXT.md`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MemCacheKey {
    identity: Option<RequestIdentity>,
    key: ResourceKey,
}

impl MemCacheKey {
    fn new(key: &ResourceKey, identity: Option<&RequestIdentity>) -> Self {
        Self {
            key: key.clone(),
            identity: identity.cloned(),
        }
    }
}

/// In-memory [`Assets`] implementation.
///
/// Shares existing [`MemResource`] instances for the same composite key
/// (`asset_root`, `ResourceKey`, `RequestIdentity`) via an internal weak
/// cache. Distinct `asset_roots` stay isolated by construction.
#[derive(Clone, Debug)]
pub struct MemAssetStore {
    /// Weak cache of active resources to ensure sharing.
    active_resources: Arc<DashMap<MemCacheKey, Weak<StorageResource>>>,
    /// Single canonical removal channel. Synchronises in-memory
    /// `active_resources` clearing with the [`AvailabilityIndex`].
    /// See [`crate::deleter`].
    deleter: Arc<dyn AssetDeleter>,
    availability: AvailabilityIndex,
    cancel: CancelToken,
    mem_resource_capacity: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct MemAssetDeleter {
    active_resources: Arc<DashMap<MemCacheKey, Weak<StorageResource>>>,
    availability: AvailabilityIndex,
    lru: crate::index::LruIndex,
    pins: crate::index::PinsIndex,
}

impl MemAssetDeleter {
    pub(crate) fn new(
        availability: AvailabilityIndex,
        pins: crate::index::PinsIndex,
        lru: crate::index::LruIndex,
        active_resources: Arc<DashMap<MemCacheKey, Weak<StorageResource>>>,
    ) -> Self {
        Self {
            active_resources,
            availability,
            lru,
            pins,
        }
    }
}

impl AssetDeleter for MemAssetDeleter {
    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        self.active_resources
            .retain(|k, _| k.key.asset_root() != Some(asset_root));
        self.availability.clear_root(asset_root);
        let pins_result = self.pins.remove(asset_root).map(|_| ());
        let lru_result = self.lru.remove(asset_root);
        pins_result.and(lru_result)
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.active_resources.retain(|k, _| k.key != *key);
        self.availability.remove(key);
        Ok(())
    }
}

/// Setup for [`MemAssetStore::with_availability_and_deleter`]: the `cancel`
/// token, optional `mem_resource_capacity`, the shared `availability` index,
/// the `active_resources` map, and the canonical `deleter`.
pub(crate) struct MemStoreSetup {
    pub(crate) active_resources: Arc<DashMap<MemCacheKey, Weak<StorageResource>>>,
    pub(crate) deleter: Arc<dyn AssetDeleter>,
    pub(crate) availability: AvailabilityIndex,
    pub(crate) cancel: CancelToken,
    pub(crate) mem_resource_capacity: Option<usize>,
}

impl MemAssetStore {
    /// Create a new in-memory asset store with its own unshared
    /// [`AvailabilityIndex`].
    #[must_use]
    pub fn new(
        cancel: CancelToken,
        mem_resource_capacity: Option<usize>,
        pool: &kithara_bufpool::BytePool,
    ) -> Self {
        Self::with_availability(
            cancel,
            mem_resource_capacity,
            AvailabilityIndex::new(),
            pool,
        )
    }

    fn scoped_observer(&self, key: &ResourceKey) -> Arc<dyn AvailabilityObserver> {
        ScopedAvailabilityObserver::new(key.clone(), self.availability.clone())
    }

    /// Like [`MemAssetStore::new`] but shares the given aggregate
    /// availability handle.
    pub(crate) fn with_availability(
        cancel: CancelToken,
        mem_resource_capacity: Option<usize>,
        availability: AvailabilityIndex,
        pool: &kithara_bufpool::BytePool,
    ) -> Self {
        let active_resources = Arc::new(DashMap::new());
        let _ = pool;
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
        let deleter: Arc<dyn AssetDeleter> = Arc::new(MemAssetDeleter::new(
            availability.clone(),
            pins,
            lru,
            Arc::clone(&active_resources),
        ));
        Self::with_availability_and_deleter(MemStoreSetup {
            active_resources,
            deleter,
            availability,
            cancel,
            mem_resource_capacity,
        })
    }

    /// Like [`Self::with_availability`] but accepts a pre-built
    /// [`AssetDeleter`] so the production builder can share the same
    /// deleter instance with the LRU evictor (`EvictAssets`).
    pub(crate) fn with_availability_and_deleter(setup: MemStoreSetup) -> Self {
        let MemStoreSetup {
            cancel,
            mem_resource_capacity,
            availability,
            active_resources,
            deleter,
        } = setup;
        Self {
            active_resources,
            deleter,
            availability,
            cancel,
            mem_resource_capacity,
        }
    }
}

impl Assets for MemAssetStore {
    type ActiveRes = BaseWriter;
    type Context = ();
    type IndexRes = StorageResource;
    type ReadyRes = BaseReader;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<BaseWriter, BaseReader>> {
        if key.rel_path().is_some_and(str::is_empty) {
            return Err(AssetsError::InvalidKey);
        }

        let cache_key = MemCacheKey::new(key, identity);
        if let Some(weak) = self.active_resources.get(&cache_key)
            && let Some(res) = weak.upgrade()
        {
            let storage = (*res).clone();
            if matches!(storage.status(), ResourceStatus::Committed { .. }) {
                return Ok(AcquisitionResult::Ready(BaseReader::new(storage)));
            }
            return Ok(AcquisitionResult::Pending(BaseWriter::new(storage)));
        }

        let mut options = MemOptions::default();
        if let Some(capacity) = self.mem_resource_capacity
            && capacity > 0
        {
            options.capacity = capacity;
        }
        let mem: MemResource = Resource::open_with_observer(
            self.cancel.clone(),
            options,
            Some(self.scoped_observer(key)),
        )
        .map_err(AssetsError::Storage)?;

        let shared = Arc::new(StorageResource::from(mem));
        self.active_resources
            .insert(cache_key, Arc::downgrade(&shared));

        Ok(AcquisitionResult::Pending(BaseWriter::new(
            (*shared).clone(),
        )))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::CACHE | Capabilities::PROCESSING
    }

    delegate::delegate! {
        to self.deleter {
            fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;
            fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
        }
        to self.cancel {
            #[expr(Ok(StorageResource::from(MemResource::new($))))]
            #[call(clone)]
            fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            #[expr(Ok(StorageResource::from(MemResource::new($))))]
            #[call(clone)]
            fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<BaseReader> {
        if key.rel_path().is_some_and(str::is_empty) {
            return Err(AssetsError::InvalidKey);
        }

        let cache_key = MemCacheKey::new(key, identity);
        if let Some(weak) = self.active_resources.get(&cache_key)
            && let Some(res) = weak.upgrade()
        {
            return Ok(BaseReader::new((*res).clone()));
        }

        Err(IoError::new(ErrorKind::NotFound, "resource missing").into())
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        if key.rel_path().is_some_and(str::is_empty) {
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
    use kithara_test_utils::kithara;

    use super::*;
    use crate::acquisition::{AcquisitionResult, ReadSide, WriteSide};

    fn make_mem_store() -> MemAssetStore {
        MemAssetStore::new(CancelToken::never(), None, &crate::BytePool::default())
    }

    fn pending(acq: AcquisitionResult<BaseWriter, BaseReader>) -> BaseWriter {
        match acq {
            AcquisitionResult::Pending(w) => w,
            AcquisitionResult::Ready(_) => panic!("expected a fresh Pending writer"),
        }
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn acquire_creates_mem_resource() {
        let store = make_mem_store();
        let key = ResourceKey::relative("test_asset", "seg_0.m4s");

        let writer = pending(store.acquire_resource(&key, None).unwrap());
        assert!(
            writer.reader().path().is_none(),
            "mem-backed resource has no on-disk path"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn write_commit_read_roundtrip() {
        let store = make_mem_store();
        let key = ResourceKey::relative("test_asset", "seg_0.m4s");

        let writer = pending(store.acquire_resource(&key, None).unwrap());
        writer.write_at(0, b"segment data").unwrap();
        let reader = writer.commit(Some(12)).unwrap();

        let mut buf = [0u8; 12];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"segment data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn no_path_for_mem_resources() {
        let store = make_mem_store();
        let key = ResourceKey::relative("test_asset", "seg_0.m4s");

        let writer = pending(store.acquire_resource(&key, None).unwrap());
        assert!(writer.reader().path().is_none());
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
