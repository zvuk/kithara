#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::env;
use std::{num::NonZeroUsize, path::PathBuf};

use dashmap::DashMap;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_config::Config;
use kithara_derive::Patch;
use kithara_events::EventBus;
use kithara_platform::{CancelScope, CancelToken, sync::Arc, time::Duration};
use serde::{Deserialize, Deserializer};

#[cfg(not(target_arch = "wasm32"))]
use super::disk::{DiskStoreSetup, fresh_temp_root, open_disk_backend};
use super::{
    OnInvalidatedFn,
    chain::StoreChain,
    handle::{AssetStore, AssetStoreInner, StoreBackendInner},
};
use crate::{
    backend::{AssetDeleter, MemAssetDeleter, MemAssetStore, MemStoreSetup},
    decorator::{
        Assets, ByteRecorder, CachedAssets, Capabilities, EvictAssets, EvictDeps, EvictionEvents,
        EvictionRouter, LeaseAssets, LeaseEvents, ProcessingAssets,
    },
    index::{AvailabilityIndex, FlushHub, PendingResourceIndex, ResourceTransactionIndex},
    layout::{AssetLayoutRegistry, ResourceKey},
};

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

/// Wire shape a configuration document spells `backend` in: `{kind: memory}`
/// or `{kind: disk, root: /tmp/kithara}`. `StorageBackend::Memory` is a unit
/// variant, so a derived `#[serde(tag = "kind")]` on `StorageBackend` itself
/// would check `deny_unknown_fields` against that variant's own (empty) field
/// list and silently drop a stray `root` next to `kind: memory`. Spelling
/// `Memory {}` on the real type would fix that but churns every call site
/// that writes `StorageBackend::Memory`. This mirror type carries the
/// `deny_unknown_fields` check instead, and `StorageBackend` stays untouched.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BackendDoc {
    Memory {},
    Disk { root: PathBuf },
}

impl<'de> Deserialize<'de> for StorageBackend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match BackendDoc::deserialize(deserializer)? {
            BackendDoc::Memory {} => Self::Memory,
            BackendDoc::Disk { root } => Self::Disk { root },
        })
    }
}

/// Everything an [`AssetStore`] opens with: the tunables a configuration
/// document may name and the wiring a caller hands over.
///
/// [`AssetStoreConfigPatch`] is what a document may say about it.
#[derive(Config, Patch)]
#[config(owner_access, builder(
    start_fn = for_pools,
    finish_fn = into_config,
    builder_type(name = AssetStoreBuilder, vis = "pub"),
    state_mod(vis = "pub")
), fields(value))]
#[non_exhaustive]
pub struct AssetStoreConfig<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Buffer-pool facade every layer of the store shares.
    #[config(
        skip = "transferred to store runtime owners",
        builder(start_fn),
        patch(skip)
    )]
    pub pools: PoolRegion<S>,
    /// Where resources live. Unset resolves to a disk root under a fresh
    /// temp directory, which is a different place on every launch.
    #[config(skip = "transferred to the selected storage backend")]
    pub backend: Option<StorageBackend>,
    /// Resources the in-memory cache retains before it evicts the
    /// least-recently-used one. Applies to both backends.
    #[config(get(copy))]
    pub cache_capacity: Option<NonZeroUsize>,
    /// Master cancel token for the store subtree.
    #[config(skip = "composed into the store cancel scope", patch(skip))]
    pub cancel: Option<CancelToken>,
    /// Event bus the eviction and lease layers publish on.
    #[config(skip = "transferred to store observers", patch(skip))]
    pub event_bus: Option<EventBus>,
    /// Shared index-flush hub. Created per store when absent.
    #[config(skip = "transferred to the flush owner", patch(skip))]
    pub flush_hub: Option<Arc<FlushHub>>,
    /// Resource-key layout registry. Empty when absent.
    #[config(skip = "transferred to the layout owner", patch(skip))]
    pub layouts: Option<AssetLayoutRegistry>,
    /// Assets the eviction policy keeps before it drops the coldest one.
    #[config(get(copy))]
    pub max_assets: Option<usize>,
    /// Bytes the eviction policy keeps before it drops the coldest asset.
    #[config(get(copy))]
    pub max_bytes: Option<u64>,
    /// Resources one in-memory asset holds. **Memory backend only** — the disk
    /// backend never reads it, so naming it beside `backend: disk` (or beside
    /// no backend at all, which resolves to disk) configures nothing.
    #[config(get(copy))]
    pub mem_resource_capacity: Option<usize>,
    /// Bytes read, transformed, and written per pass when a resource is
    /// processed on commit. Unset leaves the processing layer's own default.
    #[config(get(copy))]
    pub processing_chunk_size: Option<usize>,
    /// Recheck cadence for a reader blocked on the processing readiness gate.
    /// Unset leaves the processing layer's own default.
    #[config(patch(humantime), get(copy))]
    pub processing_gate_poll_interval: Option<Duration>,
    /// Bytes a fresh segment's temp file is reserved at. **Disk backend
    /// only** — the memory backend has no temp file to reserve. Unset leaves
    /// the disk backend's own default.
    #[config(get(copy))]
    pub segment_reservation: Option<u64>,
}

impl<S, State> AssetStoreBuilder<S, State>
where
    S: HasPool<u8> + Send + Sync + 'static,
    State: asset_store_builder::IsComplete,
{
    /// Open the store this configuration describes.
    #[must_use]
    pub fn build(self) -> AssetStore<S> {
        AssetStore::open(self.into_config())
    }
}

impl<S> AssetStore<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Start describing a store over `pools`.
    pub fn builder(pools: PoolRegion<S>) -> AssetStoreBuilder<S> {
        AssetStoreConfig::for_pools(pools)
    }

    pub(super) fn decorate_backend<A>(
        inner: Arc<A>,
        deps: EvictDeps<S>,
        on_invalidated: Option<OnInvalidatedFn>,
        volatile: bool,
    ) -> StoreChain<A, S>
    where
        A: Assets,
    {
        let records_bytes = inner.capabilities().contains(Capabilities::EVICT);
        let config = Arc::clone(&deps.config);
        let cancel = deps.cancel.clone();
        let pins = deps.pins.clone();
        let events = LeaseEvents::new(config.event_bus.clone());
        let evict = Arc::new(EvictAssets::new(inner, deps));
        let byte_recorder = records_bytes.then(|| Arc::clone(&evict) as Arc<dyn ByteRecorder>);
        let processing = Arc::new(ProcessingAssets::new(
            Arc::clone(&evict),
            Arc::clone(&config),
        ));
        let cached = Arc::new(CachedAssets::with_policy(
            processing,
            config,
            on_invalidated,
            volatile,
        ));
        LeaseAssets::with_byte_recorder(cached, cancel, byte_recorder, events, pins)
    }

    /// Open a ready-to-use asset store.
    ///
    /// The pending-resource index, eviction router, and memory-cache invalidation hook are
    /// consumer-driven siblings of `availability`, wired here without additional observer or
    /// decorator threading.
    #[must_use]
    pub fn open(config: AssetStoreConfig<S>) -> Self {
        let config = Arc::new(config);
        let backend = config.backend.clone();
        let cancel = config.cancel.clone();
        let event_bus = config.event_bus.clone();
        let layouts = config.layouts.clone();

        let availability = AvailabilityIndex::new();
        let pending_resources = PendingResourceIndex::new(CancelScope::new(cancel.clone()).token());
        let transactions = ResourceTransactionIndex::default();
        let eviction = EvictionRouter::default();
        let layouts = layouts.unwrap_or_default();

        #[cfg(not(target_arch = "wasm32"))]
        let disk_root = match backend.unwrap_or_else(|| StorageBackend::Disk {
            root: fresh_temp_root(),
        }) {
            StorageBackend::Memory => None,
            StorageBackend::Disk { root } => Some(root),
        };

        #[cfg(target_arch = "wasm32")]
        let _ = backend;

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(root_dir) = disk_root {
            return Self::new_handle(AssetStoreInner {
                config: Arc::clone(&config),
                pending_resources,
                transactions,
                eviction,
                layouts,
                backend: open_disk_backend(DiskStoreSetup {
                    config: Arc::clone(&config),
                    root_dir,
                    cancel,
                    flush_hub: config.flush_hub.clone(),
                    pools: config.pools.clone(),
                    event_bus,
                    availability: availability.clone(),
                }),
                availability,
            });
        }

        let cancel = CancelScope::new(cancel).token();
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
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
                config: Arc::clone(&config),
                cancel: cancel.clone(),
                availability: availability.clone(),
                deleter: Arc::clone(&deleter),
            },
        ));
        let availability_for_hook = availability.clone();
        let eviction_for_hook = eviction.clone();
        let on_invalidated: OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            availability_for_hook.remove(key);
            eviction_for_hook.route(key);
        });
        let store = Self::decorate_backend(
            mem,
            EvictDeps {
                lru,
                deleter,
                config: Arc::clone(&config),
                cancel,
                events: EvictionEvents::new(event_bus),
                pins,
            },
            Some(on_invalidated),
            true,
        );

        Self::new_handle(AssetStoreInner {
            config,
            availability,
            pending_resources,
            transactions,
            eviction,
            layouts,
            backend: StoreBackendInner::Memory { store },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use kithara_events::EventBus;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        AssetEvent, AssetResourceState, AssetWriter, AssetsError, EvictReason, ResourceAcquisition,
        ResourceKey, consts,
        decorator::Capabilities,
        index::FlushPolicy,
        resource::{AcquisitionResult, ReadSide, WriteSide},
    };

    type TestAssetWriter = AssetWriter<crate::test_pools::TestPools>;
    type TestResourceAcquisition = ResourceAcquisition<crate::test_pools::TestPools>;

    #[kithara::test]
    fn store_retains_config_for_effective_cache_capacity() {
        use kithara_config::ConfigOwner;

        let capacity = NonZeroUsize::new(7).unwrap();
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .cache_capacity(capacity)
            .build();
        let clone = store.clone();

        assert!(std::ptr::eq(store.config(), clone.config()));
        assert_eq!(store.config().cache_capacity, Some(capacity));
        assert_eq!(store.ephemeral_cache_capacity(), Some(capacity));
    }

    /// Stream `data` through the Pending writer and commit it.
    fn write_commit(acq: TestResourceAcquisition, data: &[u8]) {
        let AcquisitionResult::Pending(w) = acq else {
            panic!("expected a Pending writer");
        };
        w.write_at(0, data).unwrap();
        w.commit(Some(data.len() as u64)).unwrap();
    }

    /// Extract the Pending writer or panic.
    fn pending(acq: TestResourceAcquisition) -> TestAssetWriter {
        match acq {
            AcquisitionResult::Pending(w) => w,
            AcquisitionResult::Ready(_) | AcquisitionResult::Joined(_) => {
                panic!("expected a Pending writer")
            }
        }
    }

    fn collect_events(events: &mut kithara_events::EventReceiver<AssetEvent>) -> Vec<AssetEvent> {
        std::iter::from_fn(|| events.try_recv().ok())
            .map(|envelope| envelope.event)
            .collect()
    }

    #[kithara::test]
    fn memory_store_does_not_start_a_persistence_worker() {
        let hub = FlushHub::new(CancelScope::new(None).token(), FlushPolicy::default());
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .flush_hub(Arc::clone(&hub))
            .build();

        assert!(!hub.has_worker());
        drop(store);
        assert!(!hub.has_worker());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn commit_publishes_asset_committed() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .event_bus(bus)
            .build();
        let key = ResourceKey::relative(consts::BUILDER_ROOT, "seg.m4s");

        write_commit(store.acquire_resource(&key, None).unwrap(), b"data");

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            AssetEvent::Committed {
                asset_root,
                rel_path,
                final_len: Some(4),
            } if asset_root == consts::BUILDER_ROOT && rel_path == "seg.m4s"
        )));
    }

    /// A failed fetch must not cost the resource its future. The network
    /// going away for a moment is the ordinary case: the same segment is
    /// asked for again once it is back, and that request has to open a fresh
    /// download rather than trip over the wreckage of the last one.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reacquire_after_failure_opens_a_fresh_download() {
        let dir = tempdir().unwrap();
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let key = ResourceKey::relative(consts::BUILDER_ROOT, "seg.m4s");

        let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
            panic!("expected a Pending writer");
        };
        // The reader the player parks on outlives the download, which is what
        // routes the retry through the cached in-flight entry rather than a
        // clean first acquisition.
        let parked = writer.reader();
        writer.write_at(0, b"half").unwrap();
        writer.fail("network away".to_string());

        let reacquired = store
            .acquire_resource(&key, None)
            .expect("a failed segment must be fetchable again");
        assert!(
            matches!(reacquired, AcquisitionResult::Pending(_)),
            "the retry must get a writer, not the failed resource"
        );
        drop(parked);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn fail_publishes_asset_failed() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .event_bus(bus)
            .build();
        let key = ResourceKey::relative(consts::BUILDER_ROOT, "seg.m4s");
        let writer = pending(store.acquire_resource(&key, None).unwrap());

        writer.fail("fixture failure".to_string());

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            AssetEvent::Failed {
                asset_root,
                rel_path,
                reason,
            } if asset_root == consts::BUILDER_ROOT && rel_path == "seg.m4s" && reason == "fixture failure"
        )));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn quota_eviction_publishes_asset_evicted() {
        let dir = tempdir().unwrap();
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .max_assets(1)
            .event_bus(bus)
            .build();

        let key_a = ResourceKey::relative("asset-a", "seg0.m4s");
        let key_b = ResourceKey::relative("asset-b", "seg0.m4s");
        write_commit(store.acquire_resource(&key_a, None).unwrap(), b"a");
        let _ = collect_events(&mut events);
        write_commit(store.acquire_resource(&key_b, None).unwrap(), b"b");

        let events = collect_events(&mut events);
        assert!(events.iter().any(|event| matches!(
            event,
            AssetEvent::Evicted {
                asset_root,
                reason: EvictReason::QuotaAssets,
            } if asset_root == "asset-a"
        )));
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_local_mode_decorators_inactive() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        fs::write(&file_path, b"data").unwrap();

        let store = AssetStore::builder(crate::test_pools::pools())
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
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .build();

        let key = ResourceKey::absolute(&file_path).expect("absolute test path");
        let reader = store.open_resource(&key, None).unwrap();

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
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();

        let key = ResourceKey::relative(consts::BUILDER_ROOT, "test.bin");
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

        let store = AssetStore::builder(crate::test_pools::pools())
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
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .build();

        let key = ResourceKey::relative(consts::BUILDER_ROOT, "seg.m4s");
        write_commit(store.acquire_resource(&key, None).unwrap(), b"data");

        let reader = store.open_resource(&key, None).unwrap();
        let mut buf = [0u8; 4];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn disk_backend_persists_across_store_reopen() {
        let dir = tempdir().unwrap();
        let key = ResourceKey::relative(consts::BUILDER_ROOT, "seg.m4s");

        {
            let store = AssetStore::builder(crate::test_pools::pools())
                .backend(StorageBackend::Disk {
                    root: dir.path().into(),
                })
                .build();
            write_commit(store.acquire_resource(&key, None).unwrap(), b"data");
        }

        let reopened = AssetStore::builder(crate::test_pools::pools())
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
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .build();
        let caps = store.capabilities();
        assert!(!caps.contains(Capabilities::EVICT));
        assert!(!caps.contains(Capabilities::LEASE));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn disk_defaults_all_capabilities() {
        let dir = tempdir().unwrap();
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        assert_eq!(store.capabilities(), Capabilities::all());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn memory_retains_data_within_cache_capacity() {
        let backend = AssetStore::builder(crate::test_pools::pools())
            .cache_capacity(NonZeroUsize::new(5).unwrap())
            .backend(StorageBackend::Memory)
            .build();

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(consts::BUILDER_ROOT, format!("seg_{i}.m4s")))
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
        let backend = AssetStore::builder(crate::test_pools::pools())
            .cache_capacity(NonZeroUsize::new(3).unwrap())
            .backend(StorageBackend::Memory)
            .build();

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(consts::BUILDER_ROOT, format!("seg_{i}.m4s")))
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
        let backend = AssetStore::builder(crate::test_pools::pools())
            .cache_capacity(NonZeroUsize::new(128).unwrap())
            .max_bytes(8)
            .backend(StorageBackend::Memory)
            .build();
        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(consts::BUILDER_ROOT, format!("track_{i}.mp3")))
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
        let backend = AssetStore::builder(crate::test_pools::pools())
            .cache_capacity(NonZeroUsize::new(128).unwrap())
            .max_bytes(4)
            .backend(StorageBackend::Memory)
            .build();
        let key = ResourceKey::relative(consts::BUILDER_ROOT, "oversized.mp3");

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
        let store = AssetStore::builder(crate::test_pools::pools())
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

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn red_test_hydrated_index_strands_a_pruned_cache() {
        let dir = tempdir().unwrap();
        let seg_root = "seg_root";
        let target = ResourceKey::relative(seg_root, "v0_15.m4s");
        let path = dir.path().join(seg_root).join("v0_15.m4s");
        let open_store = || {
            AssetStore::builder(crate::test_pools::pools())
                .backend(StorageBackend::Disk {
                    root: dir.path().into(),
                })
                .build()
        };

        {
            let store = open_store();
            write_commit(store.acquire_resource(&target, None).unwrap(), b"data");
            assert!(store.contains_range(&target, 0..4));
            store.checkpoint().expect("persist the availability index");
        }

        assert!(
            open_store().contains_range(&target, 0..4),
            "a committed range must still be claimed after a restart"
        );

        fs::remove_file(&path).expect("prune the cached bytes");

        assert!(
            !open_store().contains_range(&target, 0..4),
            "contains_range must NOT claim a range whose file is gone. \
             The availability index is the canonical reflection of disk \
             state, and hydration reads it back without asking whether the \
             bytes are still there. Consequence in production: the HLS \
             reader spins on wait_range=Ready / read_at=Retry, no fetch is \
             ever dispatched for the slot, and the deck never loads"
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
        let store = AssetStore::builder(crate::test_pools::pools())
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

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn a_document_sets_the_cache_capacity_and_leaves_the_rest() {
        let settings: AssetStoreConfigPatch =
            serde_yaml_ng::from_str("cache_capacity: 32\n").expect("the document types");

        assert_eq!(
            settings.cache_capacity,
            Some(NonZeroUsize::new(32).expect("nonzero"))
        );
        assert_eq!(settings.max_bytes, None, "a silent knob stays unset");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn a_document_reads_the_processing_gate_poll_interval_as_humantime() {
        let settings: AssetStoreConfigPatch =
            serde_yaml_ng::from_str("processing_gate_poll_interval: 250ms\n")
                .expect("the document types");

        assert_eq!(
            settings.processing_gate_poll_interval,
            Some(Duration::from_millis(250))
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn a_document_types_a_memory_backend() {
        let settings: AssetStoreConfigPatch =
            serde_yaml_ng::from_str("backend:\n  kind: memory\n").expect("the document types");

        assert_eq!(settings.backend, Some(StorageBackend::Memory));
    }

    /// Pins ruling 108: `StorageBackend::Memory` is a unit variant, so
    /// `deny_unknown_fields` only bites if the mirror type actually spells it
    /// `Memory {}` rather than deriving on `StorageBackend` directly.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn a_memory_backend_with_a_stray_root_is_refused_and_names_it() {
        let error = serde_yaml_ng::from_str::<StorageBackend>("kind: memory\nroot: /tmp/x\n")
            .expect_err("a memory backend has no root to accept");

        assert!(error.to_string().contains("root"), "{error}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(native, flash(false))]
    fn a_disk_backend_types_its_root() {
        let backend: StorageBackend =
            serde_yaml_ng::from_str("kind: disk\nroot: /tmp/x\n").expect("a valid disk backend");

        assert_eq!(
            backend,
            StorageBackend::Disk {
                root: PathBuf::from("/tmp/x")
            }
        );
    }

    /// The plumbing end to end: a document's `cache_capacity` reaches the
    /// builder through `maybe_cache_capacity` and the store enforces it, not
    /// just the settings struct that reports it.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn a_store_built_from_the_documents_settings_enforces_the_capacity_it_named() {
        let settings: AssetStoreConfigPatch =
            serde_yaml_ng::from_str("cache_capacity: 1\n").expect("the document types");

        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .maybe_cache_capacity(settings.cache_capacity)
            .build();

        let keys: Vec<ResourceKey> = (0..2)
            .map(|i| ResourceKey::relative(consts::BUILDER_ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            write_commit(store.acquire_resource(key, None).unwrap(), b"data");
        }

        assert!(
            store.open_resource(&keys[0], None).is_err(),
            "the document's cache_capacity of 1 must evict the first \
             resource once a second one lands"
        );
        assert!(
            store.open_resource(&keys[1], None).is_ok(),
            "the resource that fits the capacity must still open -- without \
             this the eviction assertion above would also pass if opening \
             failed for some unrelated reason"
        );
    }
}
