#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::env;
use std::{fmt, num::NonZeroUsize, path::PathBuf};

use bon::{Builder, bon};
use dashmap::DashMap;
use kithara_bufpool::BytePool;
use kithara_platform::{CancelScope, CancelToken, sync::Arc};

#[cfg(not(target_arch = "wasm32"))]
use crate::disk_store::DiskAssetStore;
use crate::{
    acquisition::AcquisitionResult,
    base::{BaseReader, BaseWriter},
    cache::{CachedAssets, CachedReader, CachedWriter},
    evict::{EvictAssets, EvictDeps},
    eviction::EvictionRouter,
    flush::{FlushHub, FlushPolicy},
    index::{AvailabilityIndex, DemandIndex, EvictConfig},
    key::ResourceKey,
    layout::{AssetLayout, DefaultLayout},
    lease::{LeaseAssets, LeaseGuard, LeaseReader, LeaseWriter},
    mem_store::{MemAssetStore, MemStoreSetup},
    process::{ProcessedReader, ProcessedWriter, ProcessingAssets},
    unified::AssetStore,
};

/// Private module-level defaults, grouped per ast-grep style rule.
struct Consts;
impl Consts {
    /// Default in-memory LRU cache capacity (init + 2-3 media segments).
    const DEFAULT_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();
}

/// Hook fired when the cache volatile-displaces a resource.
pub(crate) type OnInvalidatedFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

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

/// Simplified storage options for creating an asset store; used by higher-level
/// crates (kithara-file, kithara-hls).
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct StoreOptions {
    /// In-memory LRU cache capacity for opened resources.
    pub cache_capacity: Option<NonZeroUsize>,
    /// Shared flush coordinator for the on-disk indexes (`pins.bin`,
    /// `lru.bin`, `availability.bin`).
    ///
    /// `None` — the builder creates a hub without a background worker;
    /// every mutation flushes synchronously (historical behaviour).
    /// `Some(hub)` — the caller-owned hub is reused, allowing several
    /// `AssetStore`s to share a single worker. Use
    /// [`FlushHub::with_worker`] in production for debounced /
    /// coalesced background flushing.
    pub flush_hub: Option<Arc<FlushHub>>,
    /// On-disk layout policy; `None` keeps [`DefaultLayout`].
    pub layout: Option<Arc<dyn AssetLayout>>,
    /// Maximum number of assets to keep (soft cap for LRU eviction).
    pub max_assets: Option<usize>,
    /// Maximum bytes to store (soft cap for LRU eviction).
    pub max_bytes: Option<u64>,
    /// Storage backend: in-memory, or disk rooted at a directory.
    #[builder(default)]
    pub backend: StorageBackend,
}

impl fmt::Debug for StoreOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreOptions")
            .field("backend", &self.backend)
            .field("cache_capacity", &self.cache_capacity)
            .field("flush_hub", &self.flush_hub.as_ref().map(|_| "..."))
            .field("layout", &self.layout)
            .field("max_assets", &self.max_assets)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl StoreOptions {
    /// Create options with a disk backend rooted at `root` and all other
    /// fields at their builder defaults.
    pub fn new<P>(root: P) -> Self
    where
        P: Into<PathBuf>,
    {
        Self::builder()
            .backend(StorageBackend::Disk { root: root.into() })
            .build()
    }
}

impl From<&StoreOptions> for EvictConfig {
    fn from(opts: &StoreOptions) -> Self {
        Self {
            max_assets: opts.max_assets,
            max_bytes: opts.max_bytes,
        }
    }
}

/// Fully decorated disk store chain. Processing travels per-acquire as a
/// [`ProcessCtx`](crate::ProcessCtx), so the chain is not generic over context.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type DiskStore =
    LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<DiskAssetStore>>>>;

/// Pending (writer) handle returned by the `Pending` arm of
/// [`AssetStore::acquire_resource`]. Owns the streaming write + decrypt-on-commit
/// capability; consumes itself on `commit` into an [`AssetReader`].
pub type AssetWriter = LeaseWriter<CachedWriter<ProcessedWriter<BaseWriter>>, LeaseGuard>;

/// Ready (reader) handle returned by [`AssetStore::open_resource`] and the
/// `Ready` arm of [`AssetStore::acquire_resource`]. Cheap to clone.
pub type AssetReader = LeaseReader<CachedReader<ProcessedReader<BaseReader>>, LeaseGuard>;

/// Phase-typed acquisition outcome returned by
/// [`AssetStore::acquire_resource`]: a `Pending` [`AssetWriter`] to stream and
/// commit, or a `Ready` [`AssetReader`] when the resource is already committed.
pub type AssetResource = AcquisitionResult<AssetWriter, AssetReader>;

/// In-memory asset store with disabled decorators.
///
/// Internal chain used for `AssetStore::Mem`.
pub(crate) type MemStore = LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<MemAssetStore>>>>;

/// Constructor for the ready-to-use [`AssetStore`].
///
/// One store services every asset under `root_dir`. A scope binds the
/// `asset_root` and mints self-contained keys; per-resource ops live on
/// the store.
struct AssetStoreBuildArgs {
    backend: Option<StorageBackend>,
    cache_capacity: Option<NonZeroUsize>,
    cancel: Option<CancelToken>,
    evict_config: Option<EvictConfig>,
    flush_hub: Option<Arc<FlushHub>>,
    layout: Option<Arc<dyn AssetLayout>>,
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
        evict_config: Option<EvictConfig>,
        flush_hub: Option<Arc<FlushHub>>,
        layout: Option<Arc<dyn AssetLayout>>,
        mem_resource_capacity: Option<usize>,
        #[builder(default = BytePool::default())] pool: BytePool,
    ) -> AssetStoreBuildArgs {
        AssetStoreBuildArgs {
            backend,
            cache_capacity,
            cancel,
            evict_config,
            flush_hub,
            layout,
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
    #[must_use]
    pub fn build(self) -> AssetStore {
        self.into_args().build()
    }

    /// Build a disk-backed `AssetStore` chain with a fresh availability index.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    #[must_use]
    fn build_disk(self) -> DiskStore {
        self.into_args().build_disk()
    }

    /// Build the in-memory asset store with its own
    /// unshared [`AvailabilityIndex`].
    #[cfg(test)]
    fn build_mem(self) -> MemStore {
        self.into_args().build_mem()
    }
}

impl AssetStoreBuildArgs {
    /// Build the storage backend.
    ///
    /// Selects `AssetStore::Disk` or `AssetStore::Mem` per [`StorageBackend`];
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
        // The eviction router is the third consumer-driven sibling: the
        // memory cache's `on_invalidated` hook routes evicted keys into
        // it; the store hands subscribers per `asset_root`.
        let eviction = EvictionRouter::default();
        let layout = self
            .layout
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultLayout));
        #[cfg(target_arch = "wasm32")]
        {
            let _ = self.backend.take();
            let store = self.build_mem_with_availability(&availability, &eviction);
            AssetStore::Mem {
                store,
                availability,
                demand,
                eviction,
                layout,
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let backend = self.backend.take().unwrap_or_else(|| StorageBackend::Disk {
                root: fresh_temp_root(),
            });
            match backend {
                StorageBackend::Memory => {
                    let store = self.build_mem_with_availability(&availability, &eviction);
                    AssetStore::Mem {
                        store,
                        availability,
                        demand,
                        eviction,
                        layout,
                    }
                }
                StorageBackend::Disk { root } => {
                    let (store, base) =
                        self.build_disk_with_availability(root, availability.clone());
                    AssetStore::Disk {
                        store,
                        availability,
                        demand,
                        eviction,
                        layout,
                        base: Some(base),
                    }
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
        let evict_cfg = self.evict_config.unwrap_or_default();
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

        let deleter: Arc<dyn crate::deleter::AssetDeleter> =
            Arc::new(crate::disk_store::DiskAssetDeleter::new(
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
                pins: pins.clone(),
            },
        ));
        let processing = Arc::new(ProcessingAssets::new(Arc::clone(&evict), pool.clone()));
        let capacity = self
            .cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        // Durable backing: LRU displacement is a transparent cache miss
        // (bytes survive on disk), so the cache never invalidates and no
        let cached = Arc::new(CachedAssets::new(processing, capacity, None, false));
        let byte_recorder: Option<Arc<dyn crate::evict::ByteRecorder>> =
            Some(Arc::clone(&evict) as Arc<dyn crate::evict::ByteRecorder>);
        let _ = pool;
        let chain = LeaseAssets::with_byte_recorder(cached, cancel, byte_recorder, pins);
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
        let evict_cfg = self.evict_config.unwrap_or_default();
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
        let deleter: Arc<dyn crate::deleter::AssetDeleter> =
            Arc::new(crate::mem_store::MemAssetDeleter::new(
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
                pins: pins.clone(),
            },
        ));
        let capacity = self
            .cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        let processing = Arc::new(ProcessingAssets::new(Arc::clone(&evict), pool.clone()));
        // Ephemeral backing: LRU displacement frees the bytes, so each
        // displaced key must clear availability and reach its eviction
        let availability_for_hook = availability.clone();
        let eviction_for_hook = eviction.clone();
        let on_invalidated: OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            availability_for_hook.remove(key);
            eviction_for_hook.route(key);
        });
        let cached = Arc::new(CachedAssets::new(
            processing,
            capacity,
            Some(on_invalidated),
            true,
        ));
        LeaseAssets::new(cached, cancel, pins)
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

    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        acquisition::{AcquisitionResult, ReadSide, WriteSide},
        base::{Assets, Capabilities},
        key::ResourceKey,
    };

    const ROOT: &str = "test_asset";

    /// Stream `data` through the Pending writer and commit it.
    fn write_commit(acq: AssetResource, data: &[u8]) {
        let AcquisitionResult::Pending(w) = acq else {
            panic!("expected a Pending writer");
        };
        w.write_at(0, data).unwrap();
        w.commit(Some(data.len() as u64)).unwrap();
    }

    /// Extract the Pending writer or panic.
    fn pending(acq: AssetResource) -> AssetWriter {
        match acq {
            AcquisitionResult::Pending(w) => w,
            AcquisitionResult::Ready(_) => panic!("expected a Pending writer"),
        }
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

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key, None).unwrap();

        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
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

        let key = ResourceKey::absolute(&file_path);
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

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn from_asset_store() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build_disk();
        let backend: AssetStore = store.into();
        assert!(matches!(backend, AssetStore::Disk { .. }));
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
             `inner.remove_resource` (lease.rs:487 → DiskStore) and \
             skipped `unified::AssetStore::remove_resource`, the only \
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
