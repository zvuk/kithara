#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::env;
use std::{num::NonZeroUsize, path::PathBuf};

use bon::bon;
use dashmap::DashMap;
use kithara_bufpool::BytePool;
use kithara_events::EventBus;
use kithara_platform::{CancelScope, CancelToken, sync::Arc};

#[cfg(not(target_arch = "wasm32"))]
use super::DiskStore;
use super::{
    MemStore, OnInvalidatedFn,
    handle::{AssetStore, AssetStoreInner, StoreBackendInner},
};
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::{DiskAssetDeleter, DiskAssetStore};
#[cfg(not(target_arch = "wasm32"))]
use crate::decorator::ByteRecorder;
use crate::{
    backend::{AssetDeleter, MemAssetDeleter, MemAssetStore, MemStoreSetup},
    decorator::{
        CachedAssets, EvictAssets, EvictDeps, EvictionEvents, EvictionRouter, LeaseAssets,
        LeaseEvents, ProcessingAssets,
    },
    index::{
        AvailabilityIndex, DemandIndex, EvictConfig, FlushHub, FlushPolicy,
        ResourceTransactionIndex,
    },
    layout::{AssetLayoutRegistry, ResourceKey},
};

/// Private module-level defaults, grouped per ast-grep style rule.
struct Consts;
impl Consts {
    /// Default in-memory LRU cache capacity (init + 2-3 media segments).
    const DEFAULT_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();
}

/// Storage backend selection: where committed resource bytes live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageBackend {
    /// In-memory store; contents do not survive the process.
    Memory,
    /// Disk store rooted at `root`.
    Disk {
        /// Directory holding every asset of the store.
        root: PathBuf,
    },
}

impl Default for StorageBackend {
    /// Disk under the platform temp dir; memory on wasm (no filesystem).
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::Disk {
                root: env::temp_dir().join("kithara"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self::Memory
        }
    }
}

/// Constructor for the ready-to-use [`AssetStore`].
///
/// One store services every asset under `root_dir`. A scope binds the
/// `asset_root` and mints self-contained keys; per-resource ops live on
/// the store.
struct AssetStoreBuildArgs {
    backend: Option<StorageBackend>,
    cache_capacity: Option<NonZeroUsize>,
    cancel: Option<CancelToken>,
    event_bus: Option<EventBus>,
    flush_hub: Option<Arc<FlushHub>>,
    layouts: Option<AssetLayoutRegistry>,
    max_assets: Option<usize>,
    max_bytes: Option<u64>,
    mem_resource_capacity: Option<usize>,
    pool: BytePool,
}

struct AssetStoreBuilderFactory;

#[bon]
impl AssetStoreBuilderFactory {
    #[builder(
        start_fn(name = builder, vis = "pub(crate)"),
        builder_type(name = AssetStoreBuilder, vis = "pub"),
        state_mod(name = asset_store_builder, vis = "pub"),
        finish_fn(name = into_args, vis = "pub(crate)")
    )]
    fn args(
        backend: Option<StorageBackend>,
        cache_capacity: Option<NonZeroUsize>,
        cancel: Option<CancelToken>,
        event_bus: Option<EventBus>,
        flush_hub: Option<Arc<FlushHub>>,
        layouts: Option<AssetLayoutRegistry>,
        max_assets: Option<usize>,
        max_bytes: Option<u64>,
        mem_resource_capacity: Option<usize>,
        #[builder(default = BytePool::default())] pool: BytePool,
    ) -> AssetStoreBuildArgs {
        AssetStoreBuildArgs {
            backend,
            cache_capacity,
            cancel,
            event_bus,
            flush_hub,
            layouts,
            max_assets,
            max_bytes,
            mem_resource_capacity,
            pool,
        }
    }
}

impl Default for AssetStoreBuilder<asset_store_builder::Empty> {
    fn default() -> Self {
        AssetStoreBuilderFactory::builder()
    }
}

impl<State> AssetStoreBuilder<State>
where
    State: asset_store_builder::IsComplete,
{
    delegate::delegate! {
        to self {
            #[must_use]
            #[expr($.build())]
            #[call(into_args)]
            pub fn build(self) -> AssetStore;
            /// Build a disk-backed `AssetStore` chain with a fresh availability index.
            #[cfg(all(test, not(target_arch = "wasm32")))]
            #[must_use]
            #[expr($.build_disk())]
            #[call(into_args)]
            fn build_disk(self) -> DiskStore;
            /// Build the in-memory asset store with its own
            /// unshared [`AvailabilityIndex`].
            #[cfg(test)]
            #[expr($.build_mem())]
            #[call(into_args)]
            fn build_mem(self) -> MemStore;
        }
    }
}

impl AssetStoreBuildArgs {
    /// Build the storage backend.
    ///
    /// Selects the disk or memory backend per [`StorageBackend`];
    /// on wasm the store is always memory-backed. Creates a
    /// single [`AvailabilityIndex`] per build call and threads it
    /// through both the base store (observer target) and the enum
    /// variant (query target) so writes observed by any resource
    /// become visible through `AssetStore::contains_range`.
    #[must_use]
    fn build(mut self) -> AssetStore {
        let availability = AvailabilityIndex::new();
        // The demand index is a consumer-driven sibling of `availability`:
        // no observer / decorator threading, just a shared field. Each
        // slot's `producer_cancel` is a child of this store cancel.
        let demand = DemandIndex::new(CancelScope::new(self.cancel.clone()).token());
        let transactions = ResourceTransactionIndex::default();
        // The eviction router is the third consumer-driven sibling: the
        // memory cache's `on_invalidated` hook routes evicted keys into
        // it; the store hands subscribers per `asset_root`.
        let eviction = EvictionRouter::default();
        let layouts = self.layouts.take().unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        {
            let _ = self.backend.take();
            let store = self.build_mem_with_availability(&availability, &eviction);
            AssetStore::new_handle(AssetStoreInner {
                backend: StoreBackendInner::Memory { store },
                availability,
                demand,
                transactions,
                eviction,
                layouts,
            })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let backend = self.backend.take().unwrap_or_else(|| StorageBackend::Disk {
                root: fresh_temp_root(),
            });
            match backend {
                StorageBackend::Memory => {
                    let store = self.build_mem_with_availability(&availability, &eviction);
                    AssetStore::new_handle(AssetStoreInner {
                        backend: StoreBackendInner::Memory { store },
                        availability,
                        demand,
                        transactions,
                        eviction,
                        layouts,
                    })
                }
                StorageBackend::Disk { root } => {
                    let (store, base) =
                        self.build_disk_with_availability(root, availability.clone());
                    AssetStore::new_handle(AssetStoreInner {
                        backend: StoreBackendInner::Disk {
                            store,
                            base: Some(base),
                        },
                        availability,
                        demand,
                        transactions,
                        eviction,
                        layouts,
                    })
                }
            }
        }
    }

    /// Build a disk-backed `AssetStore` chain with a fresh availability index.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    #[must_use]
    fn build_disk(mut self) -> DiskStore {
        let root = match self.backend.take() {
            Some(StorageBackend::Disk { root }) => root,
            _ => fresh_temp_root(),
        };
        let (chain, _base) = self.build_disk_with_availability(root, AvailabilityIndex::new());
        chain
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build_disk_with_availability(
        self,
        root_dir: PathBuf,
        availability: AvailabilityIndex,
    ) -> (DiskStore, Arc<DiskAssetStore>) {
        let evict_cfg = EvictConfig {
            max_assets: self.max_assets,
            max_bytes: self.max_bytes,
        };
        let cancel = CancelScope::new(self.cancel).token();

        let pool = self.pool;

        let hub = self
            .flush_hub
            .clone()
            .unwrap_or_else(|| FlushHub::new(cancel.child(), FlushPolicy::default()));

        let pins = open_disk_pins_index(&root_dir, &cancel, &pool);
        let lru = open_disk_lru_index(&root_dir, &cancel, &pool);
        pins.attach_to(&hub);
        lru.attach_to(&hub);

        let deleter: Arc<dyn AssetDeleter> = Arc::new(DiskAssetDeleter::new(
            root_dir.clone(),
            availability.clone(),
            pins.clone(),
            lru.clone(),
        ));

        if let Some(path) = lazy_index_path(&root_dir, "availability.bin") {
            availability.enable_persistence(path, cancel.clone());
        }
        availability.attach_to(&hub);

        let disk = Arc::new(DiskAssetStore::with_availability_and_deleter(
            root_dir,
            cancel.clone(),
            availability,
            Arc::clone(&deleter),
        ));
        let base = Arc::clone(&disk);
        let evict = Arc::new(EvictAssets::new(
            disk,
            EvictDeps {
                lru,
                deleter,
                cfg: evict_cfg,
                cancel: cancel.clone(),
                events: EvictionEvents::new(self.event_bus.clone()),
                pins: pins.clone(),
            },
        ));
        let processing = Arc::new(ProcessingAssets::new(Arc::clone(&evict), pool));
        let capacity = self
            .cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        // Disk bytes survive LRU displacement, so it needs no invalidation hook.
        let cached = Arc::new(CachedAssets::new(processing, capacity, None, false));
        let byte_recorder: Option<Arc<dyn ByteRecorder>> =
            Some(Arc::clone(&evict) as Arc<dyn ByteRecorder>);
        let chain = LeaseAssets::with_byte_recorder(
            cached,
            cancel,
            byte_recorder,
            LeaseEvents::new(self.event_bus.clone()),
            pins,
        );
        (chain, base)
    }

    /// Build the in-memory asset store with its own
    /// unshared [`AvailabilityIndex`].
    #[cfg(test)]
    fn build_mem(self) -> MemStore {
        self.build_mem_with_availability(&AvailabilityIndex::new(), &EvictionRouter::default())
    }

    fn build_mem_with_availability(
        self,
        availability: &AvailabilityIndex,
        eviction: &EvictionRouter,
    ) -> MemStore {
        let cancel = CancelScope::new(self.cancel).token();
        let evict_cfg = EvictConfig {
            max_assets: self.max_assets,
            max_bytes: self.max_bytes,
        };
        let pool = self.pool;

        let hub = self
            .flush_hub
            .clone()
            .unwrap_or_else(|| FlushHub::new(cancel.child(), FlushPolicy::default()));
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
        pins.attach_to(&hub);
        lru.attach_to(&hub);
        let active_resources = Arc::new(DashMap::new());
        let deleter: Arc<dyn AssetDeleter> = Arc::new(MemAssetDeleter::new(
            availability.clone(),
            pins.clone(),
            lru.clone(),
            Arc::clone(&active_resources),
        ));
        let mem = Arc::new(MemAssetStore::with_availability_and_deleter(
            MemStoreSetup {
                active_resources,
                cancel: cancel.clone(),
                mem_resource_capacity: self.mem_resource_capacity,
                availability: availability.clone(),
                deleter: Arc::clone(&deleter),
                pool: pool.clone(),
            },
        ));
        let evict = Arc::new(EvictAssets::new(
            mem,
            EvictDeps {
                lru,
                deleter,
                cfg: evict_cfg,
                cancel: cancel.clone(),
                events: EvictionEvents::new(self.event_bus.clone()),
                pins: pins.clone(),
            },
        ));
        let capacity = self
            .cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        let processing = Arc::new(ProcessingAssets::new(Arc::clone(&evict), pool));
        // Memory bytes do not survive displacement, so indexes must be invalidated.
        let availability_for_hook = availability.clone();
        let eviction_for_hook = eviction.clone();
        let on_invalidated: OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            availability_for_hook.remove(key);
            eviction_for_hook.route(key);
        });
        let cached = Arc::new(CachedAssets::with_max_bytes(
            processing,
            capacity,
            Some(on_invalidated),
            true,
            self.max_bytes,
        ));
        LeaseAssets::with_byte_recorder(
            cached,
            cancel,
            None,
            LeaseEvents::new(self.event_bus.clone()),
            pins,
        )
    }
}

/// Unique throwaway disk root used when the builder gets no backend.
#[cfg(not(target_arch = "wasm32"))]
fn fresh_temp_root() -> PathBuf {
    tempfile::tempdir()
        .expect("BUG: failed to create AssetStore temp dir")
        .keep()
}

/// Open `_index/pins.bin` as a disk-backed [`crate::index::PinsIndex`]; on path
/// failure falls back to an ephemeral index (best-effort, lazily materialised).
#[cfg(not(target_arch = "wasm32"))]
fn open_disk_pins_index(
    root_dir: &std::path::Path,
    cancel: &CancelToken,
    pool: &BytePool,
) -> crate::index::PinsIndex {
    let Some(path) = lazy_index_path(root_dir, "pins.bin") else {
        return crate::index::PinsIndex::ephemeral();
    };
    crate::index::PinsIndex::with_persist_at(path, cancel.clone(), pool)
}

/// Open `_index/lru.bin` as a disk-backed [`crate::index::LruIndex`].
/// Same fallback policy and lazy-materialisation contract as
/// [`open_disk_pins_index`].
#[cfg(not(target_arch = "wasm32"))]
fn open_disk_lru_index(
    root_dir: &std::path::Path,
    cancel: &CancelToken,
    pool: &BytePool,
) -> crate::index::LruIndex {
    let Some(path) = lazy_index_path(root_dir, "lru.bin") else {
        return crate::index::LruIndex::ephemeral();
    };
    crate::index::LruIndex::with_persist_at(path, cancel.clone(), pool)
}

/// Build the `root_dir/_index/<name>` path; `None` if the parent dir can't be
/// created (caller falls back to an ephemeral index).
#[cfg(not(target_arch = "wasm32"))]
fn lazy_index_path(root_dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let path = root_dir.join("_index").join(name);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::debug!("create _index dir failed: {e}");
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use kithara_events::{AssetEvent, Event, EventBus, EvictReason};
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        AssetResourceState, AssetWriter, AssetsError, ResourceAcquisition, ResourceKey,
        decorator::{Assets, Capabilities},
        resource::{AcquisitionResult, ReadSide, WriteSide},
    };

    const ROOT: &str = "test_asset";

    /// Stream `data` through the Pending writer and commit it.
    fn write_commit(acq: ResourceAcquisition, data: &[u8]) {
        let AcquisitionResult::Pending(w) = acq else {
            panic!("expected a Pending writer");
        };
        w.write_at(0, data).unwrap();
        w.commit(Some(data.len() as u64)).unwrap();
    }

    /// Extract the Pending writer or panic.
    fn pending(acq: ResourceAcquisition) -> AssetWriter {
        match acq {
            AcquisitionResult::Pending(w) => w,
            AcquisitionResult::Ready(_) => panic!("expected a Pending writer"),
        }
    }

    fn collect_events(events: &mut kithara_events::EventReceiver) -> Vec<Event> {
        std::iter::from_fn(|| events.try_recv().ok())
            .map(|envelope| envelope.event)
            .collect()
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn commit_publishes_asset_committed() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .event_bus(bus.clone())
            .build();
        let key = ResourceKey::relative(ROOT, "seg.m4s");

        write_commit(store.acquire_resource(&key, None).unwrap(), b"data");

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Asset(AssetEvent::Committed {
                asset_root,
                rel_path,
                final_len: Some(4),
            }) if asset_root == ROOT && rel_path == "seg.m4s"
        )));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn fail_publishes_asset_failed() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .event_bus(bus.clone())
            .build();
        let key = ResourceKey::relative(ROOT, "seg.m4s");
        let writer = pending(store.acquire_resource(&key, None).unwrap());

        writer.fail("fixture failure".to_string());

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Asset(AssetEvent::Failed {
                asset_root,
                rel_path,
                reason,
            }) if asset_root == ROOT && rel_path == "seg.m4s" && reason == "fixture failure"
        )));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn quota_eviction_publishes_asset_evicted() {
        let dir = tempdir().unwrap();
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .max_assets(1)
            .event_bus(bus.clone())
            .build();

        let key_a = ResourceKey::relative("asset-a", "seg0.m4s");
        let key_b = ResourceKey::relative("asset-b", "seg0.m4s");
        write_commit(store.acquire_resource(&key_a, None).unwrap(), b"a");
        let _ = collect_events(&mut events);
        write_commit(store.acquire_resource(&key_b, None).unwrap(), b"b");

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Asset(AssetEvent::Evicted {
                asset_root,
                reason: EvictReason::QuotaAssets,
            }) if asset_root == "asset-a"
        )));
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_local_mode_decorators_inactive() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        fs::write(&file_path, b"data").unwrap();

        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();

        let key = ResourceKey::absolute(&file_path).expect("absolute test path");
        let res = store.open_resource(&key, None).unwrap();

        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
        assert!(matches!(
            store.remove_resource(&key),
            Err(AssetsError::InvalidKey)
        ));
        assert_eq!(fs::read(&file_path).unwrap(), b"data");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn memory_backend_opens_absolute_file_in_place() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        fs::write(&file_path, b"data").unwrap();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build();

        let key = ResourceKey::absolute(&file_path).expect("absolute test path");
        let reader = store.open_resource_with_ctx(&key, None, None).unwrap();

        let mut buf = [0u8; 4];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
        assert_eq!(reader.path(), Some(file_path.as_path()));
        assert!(matches!(
            store.resource_state(&key).unwrap(),
            AssetResourceState::Committed { final_len: Some(4) }
        ));

        let AcquisitionResult::Ready(acquired) = store.acquire_resource(&key, None).unwrap() else {
            panic!("absolute file must be acquired read-only");
        };
        let mut acquired_buf = [0u8; 4];
        let acquired_n = acquired.read_at(0, &mut acquired_buf).unwrap();
        assert_eq!(acquired_n, 4);
        assert_eq!(&acquired_buf, b"data");
        assert!(acquired.reactivate().is_err());
        assert_eq!(fs::read(&file_path).unwrap(), b"data");

        assert!(matches!(
            store.remove_resource(&key),
            Err(AssetsError::InvalidKey)
        ));
        assert_eq!(fs::read(&file_path).unwrap(), b"data");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_defaults_all_enabled() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();

        let key = ResourceKey::relative(ROOT, "test.bin");
        let writer = pending(store.acquire_resource(&key, None).unwrap());
        writer.write_at(0, b"hello").unwrap();

        let reader = writer.reader();
        let mut buf = [0u8; 5];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_absolute_key_via_arbitrary_root() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        fs::write(&file_path, b"test data").unwrap();

        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();

        let key = ResourceKey::absolute(&file_path).expect("absolute test path");
        let res = store.open_resource(&key, None).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"test data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_backend_serves_reads_without_a_disk_root() {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build();

        let key = ResourceKey::relative(ROOT, "seg.m4s");
        write_commit(store.acquire_resource(&key, None).unwrap(), b"data");

        let reader = store.open_resource(&key, None).unwrap();
        let mut buf = [0u8; 4];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn disk_backend_persists_across_store_reopen() {
        let dir = tempdir().unwrap();
        let key = ResourceKey::relative(ROOT, "seg.m4s");

        {
            let store = AssetStoreBuilder::default()
                .backend(StorageBackend::Disk {
                    root: dir.path().into(),
                })
                .build();
            write_commit(store.acquire_resource(&key, None).unwrap(), b"data");
        }

        let reopened = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let AcquisitionResult::Ready(reader) = reopened.acquire_resource(&key, None).unwrap()
        else {
            panic!("committed resource must survive a store reopen over the same root");
        };
        let mut buf = [0u8; 4];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_capabilities_lack_evict_and_lease() {
        let store = AssetStoreBuilder::default().build_mem();
        let caps = store.capabilities();
        assert!(caps.contains(Capabilities::CACHE));
        assert!(caps.contains(Capabilities::PROCESSING));
        assert!(!caps.contains(Capabilities::EVICT));
        assert!(!caps.contains(Capabilities::LEASE));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn disk_defaults_all_capabilities() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build_disk();
        assert_eq!(store.capabilities(), Capabilities::all());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_retains_data_within_cache_capacity() {
        let backend = AssetStoreBuilder::default()
            .cache_capacity(NonZeroUsize::new(5).unwrap())
            .backend(StorageBackend::Memory)
            .build();

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            write_commit(backend.acquire_resource(key, None).unwrap(), b"data");
        }

        let reopened = backend.open_resource(&keys[0], None).unwrap();
        assert_eq!(
            reopened.len(),
            Some(4),
            "resource within cache capacity must retain data"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_evicts_data_beyond_cache_capacity() {
        let backend = AssetStoreBuilder::default()
            .cache_capacity(NonZeroUsize::new(3).unwrap())
            .backend(StorageBackend::Memory)
            .build();

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            write_commit(backend.acquire_resource(key, None).unwrap(), b"data");
        }

        assert!(
            backend.open_resource(&keys[0], None).is_err(),
            "evicted resource should be gone in the memory backend"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_max_bytes_bounds_large_handle_cache() {
        let backend = AssetStoreBuilder::default()
            .cache_capacity(NonZeroUsize::new(128).unwrap())
            .max_bytes(8)
            .backend(StorageBackend::Memory)
            .build();
        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(ROOT, format!("track_{i}.mp3")))
            .collect();

        for key in &keys {
            write_commit(backend.acquire_resource(key, None).unwrap(), b"12345678");
        }

        assert!(backend.open_resource(&keys[0], None).is_err());
        assert!(backend.open_resource(&keys[1], None).is_err());
        assert_eq!(
            backend.open_resource(&keys[2], None).unwrap().len(),
            Some(8)
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_max_bytes_does_not_retain_oversized_resource() {
        let backend = AssetStoreBuilder::default()
            .cache_capacity(NonZeroUsize::new(128).unwrap())
            .max_bytes(4)
            .backend(StorageBackend::Memory)
            .build();
        let key = ResourceKey::relative(ROOT, "oversized.mp3");

        write_commit(backend.acquire_resource(&key, None).unwrap(), b"12345678");

        assert!(backend.open_resource(&key, None).is_err());
    }

    /// Pins the `local_queue_playlist_behavior_*` HLS+AES128 hang: a single-resource
    /// disk deletion via `LeaseResource::drop` must invalidate `AvailabilityIndex`
    /// synchronously. Otherwise `contains_range` keeps claiming a committed range
    /// whose file is gone, and the HLS reader spins on `wait_range=Ready` / `read_at=Retry`
    /// until the hang detector fires.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn red_test_lease_resource_drop_strands_availability_index() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let seg_root = "seg_root";

        let target = ResourceKey::relative(seg_root, "v0_15.m4s");

        write_commit(store.acquire_resource(&target, None).unwrap(), b"data");
        assert!(store.contains_range(&target, 0..4));
        let path = dir.path().join(seg_root).join("v0_15.m4s");
        assert!(path.exists(), "file must exist after commit");

        {
            let AcquisitionResult::Ready(reader) = store.acquire_resource(&target, None).unwrap()
            else {
                panic!("committed resource must acquire as Ready");
            };
            let _writer2 = reader.reactivate().expect("BUG: reactivate committed");
            // dropped without commit → LeaseWriter cleanup removes the file
        }

        assert!(
            !path.exists(),
            "LeaseResource::drop must have removed the file via inner.remove_resource — \
             this confirms the bypass path before the divergence assertion"
        );
        assert!(
            !store.contains_range(&target, 0..4),
            "contains_range must NOT claim the range is ready after \
             LeaseResource::drop deletes the on-disk file. \
             AvailabilityIndex is the canonical reflection of disk \
             state; the deletion path went through \
             `inner.remove_resource` (LeaseAssets → DiskStore) and \
             skipped `store::AssetStore::remove_resource`, the only \
             place that calls `availability.remove`. Consequence in \
             production: HLS reader spins on wait_range=Ready / \
             read_at=Retry until hang_detector fires"
        );
    }

    /// Pins the second bypass behind the same HLS+AES128 hang: deleting a whole
    /// `asset_root` directory (`delete_asset` and the two LRU-eviction paths) must
    /// also clear the per-resource `AvailabilityIndex` entries. Otherwise stale
    /// `contains_range`/`final_len` answers strand a deleted resource and the reader
    /// spins on `wait_range=Ready` / `read_at=Retry` until the hang detector fires.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn red_test_delete_asset_strands_availability_index() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let seg_root = "seg_root";

        let key_a = ResourceKey::relative(seg_root, "v0_15.m4s");
        let key_b = ResourceKey::relative(seg_root, "v0_16.m4s");

        for (key, payload) in [(&key_a, &b"aaaa"[..]), (&key_b, &b"bbbbb"[..])] {
            write_commit(store.acquire_resource(key, None).unwrap(), payload);
        }
        assert!(store.contains_range(&key_a, 0..4));
        assert!(store.contains_range(&key_b, 0..5));

        let path_a = dir.path().join(seg_root).join("v0_15.m4s");
        let path_b = dir.path().join(seg_root).join("v0_16.m4s");
        assert!(path_a.exists());
        assert!(path_b.exists());

        store.delete_asset(seg_root).unwrap();

        assert!(!path_a.exists(), "delete_asset must remove file A");
        assert!(!path_b.exists(), "delete_asset must remove file B");

        assert!(
            !store.contains_range(&key_a, 0..4),
            "contains_range(key_a) must NOT claim the range is ready \
             after delete_asset. AvailabilityIndex still holds \
             final_len/ranges for v0_15.m4s under `seg_root` because \
             `delete_asset_dir` removes the directory without touching \
             the per-resource availability map. Consequence in \
             production: HLS reader spins on wait_range=Ready / \
             read_at=Retry until hang_detector fires (the parallel \
             `local_queue_playlist_behavior_symphonia` symptom)"
        );
        assert!(
            !store.contains_range(&key_b, 0..5),
            "contains_range(key_b) must NOT claim the range is ready \
             after delete_asset. Same divergence as key_a — directory \
             gone, per-resource entries stranded in AvailabilityIndex"
        );
        assert_eq!(
            store.final_len(&key_a),
            None,
            "final_len(key_a) must be None after delete_asset — \
             AvailabilityIndex must reflect that no bytes exist"
        );
        assert_eq!(
            store.final_len(&key_b),
            None,
            "final_len(key_b) must be None after delete_asset"
        );
    }
}
