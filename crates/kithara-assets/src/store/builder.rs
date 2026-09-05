#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::env;
use std::{num::NonZeroUsize, path::PathBuf};

use bon::Builder;
use dashmap::DashMap;
use kithara_bufpool::{ByteBuffer, HasPool, PoolRegion};
use kithara_events::EventBus;
use kithara_macros::Patch;
use kithara_platform::{CancelScope, CancelToken, sync::Arc, time::Duration};
use serde::{Deserialize, Deserializer};

use super::{
    OnInvalidatedFn,
    handle::{AssetStore, AssetStoreInner, StoreBackendInner},
};
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::{DiskAssetDeleter, DiskAssetStore, indexed_path};
#[cfg(not(target_arch = "wasm32"))]
use crate::decorator::ByteRecorder;
use crate::{
    backend::{AssetDeleter, MemAssetDeleter, MemAssetStore, MemStoreSetup},
    decorator::{
        CachedAssets, EvictAssets, EvictDeps, EvictionEvents, EvictionRouter, LeaseAssets,
        LeaseEvents, ProcessingAssets,
    },
    index::{
        AvailabilityIndex, EvictConfig, FlushHub, FlushPolicy, PendingResourceIndex,
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
#[derive(Builder, Patch)]
#[builder(
    start_fn = for_pools,
    finish_fn = into_config,
    builder_type(name = AssetStoreBuilder, vis = "pub"),
    state_mod(vis = "pub")
)]
#[non_exhaustive]
pub struct AssetStoreConfig<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Buffer-pool facade every layer of the store shares.
    #[builder(start_fn)]
    #[patch(skip)]
    pub pools: PoolRegion<S>,
    /// Master cancel token for the store subtree.
    #[patch(skip)]
    pub cancel: Option<CancelToken>,
    /// Event bus the eviction and lease layers publish on.
    #[patch(skip)]
    pub event_bus: Option<EventBus>,
    /// Shared index-flush hub. Created per store when absent.
    #[patch(skip)]
    pub flush_hub: Option<Arc<FlushHub>>,
    /// Resource-key layout registry. Empty when absent.
    #[patch(skip)]
    pub layouts: Option<AssetLayoutRegistry>,
    /// Where resources live. Unset resolves to a disk root under a fresh
    /// temp directory, which is a different place on every launch.
    pub backend: Option<StorageBackend>,
    /// Resources the in-memory cache retains before it evicts the
    /// least-recently-used one. Applies to both backends.
    pub cache_capacity: Option<NonZeroUsize>,
    /// Assets the eviction policy keeps before it drops the coldest one.
    pub max_assets: Option<usize>,
    /// Bytes the eviction policy keeps before it drops the coldest asset.
    pub max_bytes: Option<u64>,
    /// Resources one in-memory asset holds. **Memory backend only** — the disk
    /// backend never reads it, so naming it beside `backend: disk` (or beside
    /// no backend at all, which resolves to disk) configures nothing.
    pub mem_resource_capacity: Option<usize>,
    /// Bytes read, transformed, and written per pass when a resource is
    /// processed on commit. Unset leaves the processing layer's own default.
    pub processing_chunk_size: Option<usize>,
    /// Recheck cadence for a reader blocked on the processing readiness gate.
    /// Unset leaves the processing layer's own default.
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub processing_gate_poll_interval: Option<Duration>,
    /// Bytes a fresh segment's temp file is reserved at. **Disk backend
    /// only** — the memory backend has no temp file to reserve. Unset leaves
    /// the disk backend's own default.
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

    /// Open a ready-to-use asset store.
    #[must_use]
    pub fn open(config: AssetStoreConfig<S>) -> Self {
        let AssetStoreConfig {
            pools,
            backend,
            cache_capacity,
            cancel,
            event_bus,
            flush_hub,
            layouts,
            max_assets,
            max_bytes,
            mem_resource_capacity,
            processing_chunk_size,
            processing_gate_poll_interval,
            segment_reservation,
        } = config;

        let availability = AvailabilityIndex::new();
        // The pending-resource index is a consumer-driven sibling of `availability`:
        // no observer / decorator threading, just a shared field. Each
        // slot's `writer_cancel` is a child of this store cancel.
        let pending_resources = PendingResourceIndex::new(CancelScope::new(cancel.clone()).token());
        let transactions = ResourceTransactionIndex::default();
        // The eviction router is the third consumer-driven sibling: the
        // memory cache's `on_invalidated` hook routes evicted keys into
        // it; the store hands subscribers per `asset_root`.
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
        let _ = (backend, segment_reservation);

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(root_dir) = disk_root {
            return Self::new_handle(AssetStoreInner {
                pending_resources,
                transactions,
                eviction,
                layouts,
                backend: open_disk_backend(DiskStoreSetup {
                    root_dir,
                    cancel,
                    flush_hub,
                    pools,
                    event_bus,
                    cache_capacity,
                    availability: availability.clone(),
                    evict_cfg: EvictConfig {
                        max_assets,
                        max_bytes,
                    },
                    processing_chunk_size,
                    processing_gate_poll_interval,
                    segment_reservation,
                }),
                availability,
            });
        }

        let cancel = CancelScope::new(cancel).token();
        let evict_cfg = EvictConfig {
            max_assets,
            max_bytes,
        };
        let hub =
            flush_hub.unwrap_or_else(|| FlushHub::new(cancel.child(), FlushPolicy::default()));
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
                mem_resource_capacity,
                availability: availability.clone(),
                deleter: Arc::clone(&deleter),
                pools: pools.clone(),
            },
        ));
        let evict = Arc::new(EvictAssets::new(
            mem,
            EvictDeps {
                lru,
                deleter,
                cfg: evict_cfg,
                cancel: cancel.clone(),
                events: EvictionEvents::new(event_bus.clone()),
                pins: pins.clone(),
            },
        ));
        let capacity = cache_capacity.unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        let processing_assets = Arc::new(ProcessingAssets::new(
            Arc::clone(&evict),
            pools,
            processing_chunk_size,
            processing_gate_poll_interval,
        ));
        // Memory bytes do not survive displacement, so indexes must be invalidated.
        let availability_for_hook = availability.clone();
        let eviction_for_hook = eviction.clone();
        let on_invalidated: OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            availability_for_hook.remove(key);
            eviction_for_hook.route(key);
        });
        let cached = Arc::new(CachedAssets::with_max_bytes(
            processing_assets,
            capacity,
            Some(on_invalidated),
            true,
            max_bytes,
        ));
        let store = LeaseAssets::with_byte_recorder(
            cached,
            cancel,
            None,
            LeaseEvents::new(event_bus),
            pins,
        );

        Self::new_handle(AssetStoreInner {
            availability,
            pending_resources,
            transactions,
            eviction,
            layouts,
            backend: StoreBackendInner::Memory { store },
        })
    }
}

/// Everything [`AssetStore::open`] resolved before it picked the disk
/// branch. Mirrors [`MemStoreSetup`] on the memory side: one bundle so the
/// branch is a function instead of another sixty lines in the builder.
#[cfg(not(target_arch = "wasm32"))]
struct DiskStoreSetup<S> {
    root_dir: PathBuf,
    cancel: Option<CancelToken>,
    flush_hub: Option<Arc<FlushHub>>,
    pools: PoolRegion<S>,
    event_bus: Option<EventBus>,
    cache_capacity: Option<NonZeroUsize>,
    availability: AvailabilityIndex,
    evict_cfg: EvictConfig,
    processing_chunk_size: Option<usize>,
    processing_gate_poll_interval: Option<Duration>,
    segment_reservation: Option<u64>,
}

/// Assemble the disk decorator chain: evict over the disk store, processing
/// over that, the memory cache over that, leases on top.
#[cfg(not(target_arch = "wasm32"))]
fn open_disk_backend<S>(setup: DiskStoreSetup<S>) -> StoreBackendInner<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let DiskStoreSetup {
        root_dir,
        cancel,
        flush_hub,
        pools,
        event_bus,
        cache_capacity,
        availability,
        evict_cfg,
        processing_chunk_size,
        processing_gate_poll_interval,
        segment_reservation,
    } = setup;
    let cancel = CancelScope::new(cancel).token();
    let hub = flush_hub.unwrap_or_else(|| FlushHub::new(cancel.child(), FlushPolicy::default()));

    let pins = open_disk_pins_index(&root_dir, &cancel, pools.get::<u8>());
    let lru = open_disk_lru_index(&root_dir, &cancel, pools.get::<u8>());
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
        availability
            .retain(|root, rel| indexed_path(&root_dir, root, rel).is_some_and(|p| p.exists()));
    }
    availability.attach_to(&hub);

    let disk = Arc::new(
        DiskAssetStore::with_availability_and_deleter()
            .root_dir(root_dir)
            .cancel(cancel.clone())
            .availability(availability)
            .deleter(Arc::clone(&deleter))
            .maybe_segment_reservation(segment_reservation)
            .call(),
    );
    let base = Arc::clone(&disk);
    let evict = Arc::new(EvictAssets::new(
        disk,
        EvictDeps {
            lru,
            deleter,
            cfg: evict_cfg,
            cancel: cancel.clone(),
            events: EvictionEvents::new(event_bus.clone()),
            pins: pins.clone(),
        },
    ));
    let processing_assets = Arc::new(ProcessingAssets::new(
        Arc::clone(&evict),
        pools,
        processing_chunk_size,
        processing_gate_poll_interval,
    ));
    let capacity = cache_capacity.unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
    // Disk bytes survive LRU displacement, so it needs no invalidation hook.
    let cached = Arc::new(CachedAssets::new(processing_assets, capacity, None, false));
    let byte_recorder: Option<Arc<dyn ByteRecorder>> =
        Some(Arc::clone(&evict) as Arc<dyn ByteRecorder>);
    let store = LeaseAssets::with_byte_recorder(
        cached,
        cancel,
        byte_recorder,
        LeaseEvents::new(event_bus),
        pins,
    );

    StoreBackendInner::Disk {
        store,
        base: Some(base),
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
    buffer: ByteBuffer,
) -> crate::index::PinsIndex {
    let Some(path) = lazy_index_path(root_dir, "pins.bin") else {
        return crate::index::PinsIndex::ephemeral();
    };
    crate::index::PinsIndex::with_persist_at(path, cancel.clone(), buffer)
}

/// Open `_index/lru.bin` as a disk-backed [`crate::index::LruIndex`].
/// Same fallback policy and lazy-materialisation contract as
/// [`open_disk_pins_index`].
#[cfg(not(target_arch = "wasm32"))]
fn open_disk_lru_index(
    root_dir: &std::path::Path,
    cancel: &CancelToken,
    buffer: ByteBuffer,
) -> crate::index::LruIndex {
    let Some(path) = lazy_index_path(root_dir, "lru.bin") else {
        return crate::index::LruIndex::ephemeral();
    };
    crate::index::LruIndex::with_persist_at(path, cancel.clone(), buffer)
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
        decorator::Capabilities,
        resource::{AcquisitionResult, ReadSide, WriteSide},
    };

    const ROOT: &str = "test_asset";

    type TestAssetWriter = AssetWriter<crate::test_pools::TestPools>;
    type TestResourceAcquisition = ResourceAcquisition<crate::test_pools::TestPools>;

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
        let store = AssetStore::builder(crate::test_pools::pools())
            .backend(StorageBackend::Memory)
            .event_bus(bus)
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
        let key = ResourceKey::relative(ROOT, "seg.m4s");

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
        let store = AssetStore::builder(crate::test_pools::pools())
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
        assert!(caps.contains(Capabilities::CACHE));
        assert!(caps.contains(Capabilities::PROCESSING));
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
        let backend = AssetStore::builder(crate::test_pools::pools())
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
        let backend = AssetStore::builder(crate::test_pools::pools())
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
        let backend = AssetStore::builder(crate::test_pools::pools())
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
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
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
