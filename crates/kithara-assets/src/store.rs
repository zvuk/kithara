#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::env;
use std::{fmt, hash::Hash, num::NonZeroUsize, path::PathBuf, sync::Arc};

use derive_setters::Setters;
use kithara_bufpool::BytePool;
use kithara_storage::StorageResource;
use tokio_util::sync::CancellationToken;

use crate::cache::CachedResource;

/// Private module-level defaults, grouped per ast-grep style rule.
struct Consts;
impl Consts {
    /// Default in-memory LRU cache capacity (init + 2-3 media segments).
    const DEFAULT_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();
}

use dashmap::DashMap;

#[cfg(not(target_arch = "wasm32"))]
use crate::disk_store::DiskAssetStore;
use crate::{
    cache::CachedAssets,
    evict::EvictAssets,
    index::{AvailabilityIndex, EvictConfig, FlushHub, FlushPolicy},
    key::ResourceKey,
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
    mem_store::MemAssetStore,
    process::{ProcessChunkFn, ProcessedResource, ProcessingAssets},
    unified::AssetStore,
};

/// Callback invoked when a cached resource is invalidated (displaced from LRU cache).
///
/// In ephemeral mode this means data loss (no disk backing).
/// In disk mode the data may still be on disk but the handle is gone.
pub type OnInvalidatedFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

/// Simplified storage options for creating an asset store.
///
/// Used by higher-level crates (kithara-file, kithara-hls) for unified configuration.
/// This provides a user-friendly API that hides internal details like `asset_root`.
#[derive(Clone, Setters)]
#[setters(prefix = "with_", strip_option)]
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
    #[setters(skip)]
    pub flush_hub: Option<Arc<FlushHub>>,
    /// Maximum number of assets to keep (soft cap for LRU eviction).
    pub max_assets: Option<usize>,
    /// Maximum bytes to store (soft cap for LRU eviction).
    pub max_bytes: Option<u64>,
    /// Called when a cached resource is invalidated (displaced from LRU cache).
    #[setters(skip)]
    pub on_invalidated: Option<OnInvalidatedFn>,
    /// Directory for persistent cache storage (required).
    pub cache_dir: PathBuf,
    /// Use ephemeral (in-memory) storage instead of disk.
    ///
    /// When `true`, the asset store uses `MemAssetStore` instead of
    /// `DiskAssetStore`. Data is never written to disk.
    /// Default: `false`.
    pub ephemeral: bool,
}

impl fmt::Debug for StoreOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreOptions")
            .field("cache_dir", &self.cache_dir)
            .field("cache_capacity", &self.cache_capacity)
            .field("ephemeral", &self.ephemeral)
            .field("flush_hub", &self.flush_hub.as_ref().map(|_| "..."))
            .field("max_assets", &self.max_assets)
            .field("max_bytes", &self.max_bytes)
            .field(
                "on_invalidated",
                &self.on_invalidated.as_ref().map(|_| "..."),
            )
            .finish()
    }
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            cache_dir: env::temp_dir().join("kithara"),
            #[cfg(target_arch = "wasm32")]
            cache_dir: PathBuf::from("/kithara"),
            cache_capacity: None,
            ephemeral: false,
            flush_hub: None,
            max_assets: None,
            max_bytes: None,
            on_invalidated: None,
        }
    }
}

impl StoreOptions {
    /// Create new store options with the given cache directory.
    pub fn new<P: Into<PathBuf>>(cache_dir: P) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            cache_capacity: None,
            ephemeral: false,
            flush_hub: None,
            max_assets: None,
            max_bytes: None,
            on_invalidated: None,
        }
    }

    /// Effective LRU cache capacity (explicit or default).
    #[must_use]
    pub fn effective_cache_capacity(&self) -> NonZeroUsize {
        self.cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY)
    }

    /// Convert to internal `EvictConfig`.
    #[must_use]
    pub fn to_evict_config(&self) -> EvictConfig {
        EvictConfig {
            max_assets: self.max_assets,
            max_bytes: self.max_bytes,
        }
    }

    /// Reuse an externally-owned [`FlushHub`] for the on-disk indexes.
    ///
    /// Several stores can share the same hub — useful when one
    /// process owns multiple [`AssetStore`]s and wants a single
    /// background flush worker covering them all.
    #[must_use]
    pub fn with_flush_hub(mut self, hub: Arc<FlushHub>) -> Self {
        self.flush_hub = Some(hub);
        self
    }
}

/// Fully decorated asset store with processing layer.
///
/// ## Decorator order (inside to outside)
/// - `DiskAssetStore` (base disk I/O)
/// - `EvictAssets` (LRU eviction)
/// - `ProcessingAssets` (transformation with context, uses Default if no context)
/// - `CachedAssets` (reuses opened resources)
/// - `LeaseAssets` (RAII pinning, outermost)
///
/// Generic parameter `Ctx` is the context type for processing.
/// Use `()` (default) for no processing (`ProcessingAssets` will pass through unchanged).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type DiskStore<Ctx = ()> =
    LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<DiskAssetStore>, Ctx>>>;

/// Resource handle returned by [`AssetStore::open_resource`].
///
/// Wraps `StorageResource` with processing and lease semantics.
/// Implements `ResourceExt` for read/write/commit operations.
/// Both disk and memory variants return this same type.
pub type AssetResource<Ctx = ()> =
    LeaseResource<CachedResource<ProcessedResource<StorageResource, Ctx>>, LeaseGuard>;

/// In-memory asset store with disabled decorators.
///
/// Internal chain used for `AssetStore::Mem`.
pub(crate) type MemStore<Ctx = ()> =
    LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<MemAssetStore>, Ctx>>>;

/// Constructor for the ready-to-use [`AssetStore`].
///
/// ## Usage
/// ```ignore
/// // Without processing:
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .asset_root(Some(&asset_root_for_url(&master_url)))
///     .build();
///
/// // With processing callback:
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .asset_root(Some(&asset_root_for_url(&master_url)))
///     .process_fn(my_decrypt_callback)
///     .build();
///
/// // Local-only (absolute keys only, no asset_root):
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .asset_root(None)
///     .build();
/// ```
///
/// ## Decorator order (normative)
/// - `EvictAssets` is applied first (evaluates eviction at "asset creation time")
/// - `CachedAssets` caches opened resources in memory
/// - `LeaseAssets` provides RAII pinning for opened resources (outermost)
/// - `ProcessingAssets` (if configured) wraps resources for transformation
pub struct AssetStoreBuilder<Ctx: Clone + Hash + Eq + Send + Sync + 'static = ()> {
    asset_root: Option<String>,
    cache_capacity: Option<NonZeroUsize>,
    cancel: Option<CancellationToken>,
    evict_config: Option<EvictConfig>,
    flush_hub: Option<Arc<FlushHub>>,
    mem_resource_capacity: Option<usize>,
    on_invalidated: Option<OnInvalidatedFn>,
    pool: Option<BytePool>,
    process_fn: Option<ProcessChunkFn<Ctx>>,
    root_dir: Option<PathBuf>,
    ephemeral: bool,
}

impl Default for AssetStoreBuilder<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetStoreBuilder<()> {
    /// Builder with defaults (no `root_dir`/`asset_root`/evict/cancel/process set).
    #[must_use]
    pub fn new() -> Self {
        // Default pass-through process_fn for () - just copies input to output
        let dummy_process: ProcessChunkFn<()> =
            Arc::new(|input, output, _ctx: &mut (), _is_last| {
                output[..input.len()].copy_from_slice(input);
                Ok(input.len())
            });

        Self {
            cache_capacity: None,
            cancel: None,
            ephemeral: false,
            evict_config: None,
            flush_hub: None,
            mem_resource_capacity: None,
            on_invalidated: None,
            pool: None,
            process_fn: Some(dummy_process),
            root_dir: None,
            asset_root: None,
        }
    }
}

impl<Ctx> AssetStoreBuilder<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + fmt::Debug + 'static,
{
    /// Set the asset root identifier (e.g. from `asset_root_for_url`).
    ///
    /// Pass `None` when the store will only be used with absolute keys
    /// (e.g. local file playback). Relative keys will fail with `InvalidKey`
    /// when `asset_root` is `None`.
    pub fn asset_root(mut self, asset_root: Option<&str>) -> Self {
        self.asset_root = asset_root.map(str::to_string);
        self
    }

    /// Build the storage backend.
    ///
    /// Returns `AssetStore::Disk` for persistent storage or
    /// `AssetStore::Mem` when `ephemeral(true)` is set. Creates a
    /// single [`AvailabilityIndex`] per build call and threads it
    /// through both the base store (observer target) and the enum
    /// variant (query target) so writes observed by any resource
    /// become visible through `AssetStore::contains_range`.
    ///
    /// # Panics
    /// Panics if `process_fn` is not set.
    #[must_use]
    pub fn build(self) -> AssetStore<Ctx> {
        let availability = AvailabilityIndex::new();
        #[cfg(target_arch = "wasm32")]
        {
            let store = self.build_ephemeral_with_availability(availability.clone());
            AssetStore::Mem {
                store,
                availability,
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.ephemeral {
                let store = self.build_ephemeral_with_availability(availability.clone());
                AssetStore::Mem {
                    store,
                    availability,
                }
            } else {
                let (store, base) = self.build_disk_with_availability(availability.clone());
                AssetStore::Disk {
                    store,
                    availability,
                    base: Some(base),
                }
            }
        }
    }

    /// Build disk-backed asset store with its own unshared
    /// [`AvailabilityIndex`]. Kept for tests and the `internal`
    /// feature; production construction uses
    /// [`AssetStoreBuilder::build`] which shares the aggregate with
    /// the enum variant.
    ///
    /// # Panics
    /// Panics if `process_fn` is not set for Ctx != ().
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn build_disk(self) -> DiskStore<Ctx> {
        let (chain, _base) = self.build_disk_with_availability(AvailabilityIndex::new());
        chain
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build_disk_with_availability(
        self,
        availability: AvailabilityIndex,
    ) -> (DiskStore<Ctx>, Arc<DiskAssetStore>) {
        let root_dir = self.root_dir.unwrap_or_else(|| {
            tempfile::tempdir()
                .expect("failed to create AssetStore temp dir")
                .keep()
        });
        let asset_root = self.asset_root.unwrap_or_default();
        let evict_cfg = self.evict_config.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();

        let process_fn = self
            .process_fn
            .expect("process_fn is required for AssetStoreBuilder");

        let pool = self.pool.unwrap_or_else(|| BytePool::default().clone());

        // FlushHub coordinates flushes for all three on-disk indexes.
        // Either reuse a caller-supplied hub (shared across stores) or
        // create one without a worker (every mutation flushes
        // synchronously, matching the historical inline-flush
        // behaviour).
        let hub = self
            .flush_hub
            .clone()
            .unwrap_or_else(|| FlushHub::new(cancel.clone(), FlushPolicy::default()));

        // Disk-backed pins/lru indexes shared across the decorator
        // stack: `LeaseAssets` mutates pins on resource lifecycle,
        // `EvictAssets` reads pins / mutates lru on touch & eviction,
        // `DiskAssetDeleter` invalidates both on full-asset removal.
        // All three see the same in-memory state through Arc-cloned
        // `PinsIndex` / `LruIndex` handles.
        let pins = open_disk_pins_index(&root_dir, &cancel, &pool);
        let lru = open_disk_lru_index(&root_dir, &cancel, &pool);
        pins.attach_to(&hub);
        lru.attach_to(&hub);

        // Single canonical deleter — see [`crate::deleter`].
        let deleter: Arc<dyn crate::deleter::AssetDeleter> =
            Arc::new(crate::disk_store::DiskAssetDeleter::new(
                root_dir.clone(),
                availability.clone(),
                pins.clone(),
                lru.clone(),
            ));

        // Enable disk persistence on the aggregate before constructing
        // the store: hydrates from any existing `_index/availability.bin`
        // and caches the persist resource for later flushes through the
        // FlushHub.
        if let Some(path) = lazy_index_path(&root_dir, "availability.bin") {
            availability.enable_persistence(path, cancel.clone());
        }
        availability.attach_to(&hub);

        let disk = Arc::new(DiskAssetStore::with_availability_and_deleter(
            root_dir,
            asset_root,
            cancel.clone(),
            availability,
            Arc::clone(&deleter),
        ));
        let base = Arc::clone(&disk);
        let evict = Arc::new(EvictAssets::new(
            disk,
            evict_cfg,
            cancel.clone(),
            lru,
            pins.clone(),
            deleter,
        ));
        let processing = Arc::new(ProcessingAssets::new(
            Arc::clone(&evict),
            process_fn,
            pool.clone(),
        ));
        let capacity = self
            .cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        let cached = Arc::new(CachedAssets::new(processing, capacity, self.on_invalidated));
        let byte_recorder: Option<Arc<dyn crate::evict::ByteRecorder>> =
            Some(Arc::clone(&evict) as Arc<dyn crate::evict::ByteRecorder>);
        let _ = pool; // pool is consumed by ProcessingAssets and CachedAssets above
        let chain = LeaseAssets::with_byte_recorder(cached, cancel, byte_recorder, pins);
        (chain, base)
    }

    /// Build ephemeral (in-memory) asset store with its own
    /// unshared [`AvailabilityIndex`].
    #[cfg(test)]
    fn build_ephemeral(self) -> MemStore<Ctx> {
        self.build_ephemeral_with_availability(AvailabilityIndex::new())
    }

    fn build_ephemeral_with_availability(self, availability: AvailabilityIndex) -> MemStore<Ctx> {
        let asset_root = self.asset_root.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();
        let evict_cfg = self.evict_config.unwrap_or_default();
        let process_fn = self
            .process_fn
            .expect("process_fn is required for AssetStoreBuilder");
        let pool = self.pool.unwrap_or_else(|| BytePool::default().clone());

        let asset_root_clone = asset_root.clone();
        // Symmetric to the disk builder: single deleter shared by
        // `MemAssetStore`, `EvictAssets`, and `LeaseAssets`. Mem
        // backend uses ephemeral pins/lru indexes — disk persistence
        // makes no sense without a backing file. See [`crate::deleter`].
        let hub = self
            .flush_hub
            .clone()
            .unwrap_or_else(|| FlushHub::new(cancel.clone(), FlushPolicy::default()));
        let pins = crate::index::PinsIndex::ephemeral();
        let lru = crate::index::LruIndex::ephemeral();
        pins.attach_to(&hub);
        lru.attach_to(&hub);
        let active_resources = Arc::new(DashMap::new());
        let deleter: Arc<dyn crate::deleter::AssetDeleter> =
            Arc::new(crate::mem_store::MemAssetDeleter::new(
                asset_root.clone(),
                availability.clone(),
                pins.clone(),
                lru.clone(),
                Arc::clone(&active_resources),
            ));
        let mem = Arc::new(MemAssetStore::with_availability_and_deleter(
            asset_root,
            cancel.clone(),
            self.mem_resource_capacity,
            availability.clone(),
            active_resources,
            Arc::clone(&deleter),
        ));
        let evict = Arc::new(EvictAssets::new(
            mem,
            evict_cfg,
            cancel.clone(),
            lru,
            pins.clone(),
            deleter,
        ));
        let capacity = self
            .cache_capacity
            .unwrap_or(Consts::DEFAULT_CACHE_CAPACITY);
        let processing = Arc::new(ProcessingAssets::new(
            Arc::clone(&evict),
            process_fn,
            pool.clone(),
        ));
        let user_on_invalidated = self.on_invalidated;
        let hooked_on_invalidated: OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            availability.remove(&asset_root_clone, key);
            if let Some(ref cb) = user_on_invalidated {
                cb(key);
            }
        });
        let cached = Arc::new(CachedAssets::new(
            processing,
            capacity,
            Some(hooked_on_invalidated),
        ));
        LeaseAssets::new(cached, cancel, pins)
    }

    /// Set the in-memory LRU cache capacity for opened resources.
    #[must_use]
    pub fn cache_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.cache_capacity = Some(capacity);
        self
    }

    #[must_use]
    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Use ephemeral (in-memory) storage instead of disk.
    ///
    /// When `true`, `build()` returns `AssetStore::Mem` with auto-eviction
    /// (LRU cache removes underlying data on eviction).
    /// Default: `false`.
    #[must_use]
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    #[must_use]
    pub fn evict_config(mut self, cfg: EvictConfig) -> Self {
        self.evict_config = Some(cfg);
        self
    }

    /// Reuse an externally-owned [`FlushHub`] for the on-disk indexes.
    /// See [`StoreOptions::with_flush_hub`].
    #[must_use]
    pub fn flush_hub(mut self, hub: Arc<FlushHub>) -> Self {
        self.flush_hub = Some(hub);
        self
    }

    /// Set capacity of each in-memory resource for ephemeral backend.
    #[must_use]
    pub fn mem_resource_capacity(mut self, capacity: usize) -> Self {
        self.mem_resource_capacity = Some(capacity);
        self
    }

    /// Set callback invoked when a cached resource is invalidated.
    #[must_use]
    pub fn on_invalidated(mut self, callback: OnInvalidatedFn) -> Self {
        self.on_invalidated = Some(callback);
        self
    }

    /// Set the buffer pool (created at application startup and shared).
    #[must_use]
    pub fn pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Set the root directory for the asset store.
    pub fn root_dir<P: Into<PathBuf>>(mut self, root: P) -> Self {
        self.root_dir = Some(root.into());
        self
    }
}

/// Open `_index/pins.bin` as a disk-backed [`crate::index::PinsIndex`].
///
/// Failures (path, parent dir creation) collapse to an ephemeral
/// index — the cache is best-effort, broken state must not prevent
/// store construction. The actual mmap file is materialised lazily
/// inside [`crate::index::PinsIndex`] on the first flush, so a fresh
/// store does not touch the filesystem until a real pin happens.
#[cfg(not(target_arch = "wasm32"))]
fn open_disk_pins_index(
    root_dir: &std::path::Path,
    cancel: &CancellationToken,
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
    cancel: &CancellationToken,
    pool: &BytePool,
) -> crate::index::LruIndex {
    let Some(path) = lazy_index_path(root_dir, "lru.bin") else {
        return crate::index::LruIndex::ephemeral();
    };
    crate::index::LruIndex::with_persist_at(path, cancel.clone(), pool)
}

/// Build the on-disk path for an index file under `root_dir/_index/`.
///
/// Returns `None` when the parent directory cannot be created — the
/// caller falls back to an ephemeral index. Only the parent directory
/// is touched here; the file itself is materialised lazily on first
/// flush.
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

impl<OldCtx: Clone + Hash + Eq + Send + Sync + 'static> AssetStoreBuilder<OldCtx> {
    /// Set the processing callback for transforming resources.
    ///
    /// This changes the builder's context type.
    pub fn process_fn<NewCtx>(self, f: ProcessChunkFn<NewCtx>) -> AssetStoreBuilder<NewCtx>
    where
        NewCtx: Clone + Hash + Eq + Send + Sync + 'static,
    {
        AssetStoreBuilder {
            cache_capacity: self.cache_capacity,
            cancel: self.cancel,
            ephemeral: self.ephemeral,
            evict_config: self.evict_config,
            flush_hub: self.flush_hub,
            mem_resource_capacity: self.mem_resource_capacity,
            on_invalidated: self.on_invalidated,
            pool: self.pool,
            process_fn: Some(f),
            root_dir: self.root_dir,
            asset_root: self.asset_root,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        panic::{AssertUnwindSafe, catch_unwind},
    };

    use kithara_platform::time::Duration;
    use kithara_storage::ResourceExt;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        base::{Assets, Capabilities},
        key::ResourceKey,
    };

    fn panic_message(err: &(dyn std::any::Any + Send)) -> String {
        if let Some(msg) = err.downcast_ref::<String>() {
            return msg.clone();
        }
        if let Some(msg) = err.downcast_ref::<&str>() {
            return (*msg).to_string();
        }
        "<non-string panic>".to_string()
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_local_mode_decorators_inactive() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        fs::write(&file_path, b"data").unwrap();

        // Empty asset_root → capabilities lack CACHE/EVICT/LEASE
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .build();

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_defaults_all_enabled() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test_asset"))
            .build();

        let key = ResourceKey::new("test.bin");
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"hello").unwrap();

        let mut buf = [0u8; 5];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_write_ops_panic() {
        let store = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .ephemeral(true)
            .build();
        let key = ResourceKey::new("test.bin");

        let write_handle = store.acquire_resource(&key).unwrap();
        write_handle.write_at(0, b"x").unwrap();
        write_handle.commit(Some(1)).unwrap();
        drop(write_handle);

        let read_handle = store.open_resource(&key).unwrap();

        let err = catch_unwind(AssertUnwindSafe(|| {
            let _ = read_handle.write_at(0, b"x");
        }))
        .expect_err("write_at via open_resource must panic");
        assert!(
            panic_message(&*err).contains("write_at requires acquire_resource*"),
            "panic must point to acquire_resource"
        );

        let err = catch_unwind(AssertUnwindSafe(|| {
            read_handle.fail("boom".to_string());
        }))
        .expect_err("fail via open_resource must panic");
        assert!(
            panic_message(&*err).contains("fail requires acquire_resource*"),
            "panic must point to acquire_resource"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_commit_and_reactivate_panic() {
        let store = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .ephemeral(true)
            .build();
        let key = ResourceKey::new("test.bin");

        let write_handle = store.acquire_resource(&key).unwrap();
        write_handle.write_at(0, b"abcd").unwrap();
        write_handle.commit(Some(4)).unwrap();
        drop(write_handle);

        let read_handle = store.open_resource(&key).unwrap();

        let err = catch_unwind(AssertUnwindSafe(|| {
            let _ = read_handle.commit(Some(4));
        }))
        .expect_err("commit via open_resource must panic");
        assert!(
            panic_message(&*err).contains("commit requires acquire_resource*"),
            "panic must point to acquire_resource"
        );

        let err = catch_unwind(AssertUnwindSafe(|| {
            let _ = read_handle.reactivate();
        }))
        .expect_err("reactivate via open_resource must panic");
        assert!(
            panic_message(&*err).contains("reactivate requires acquire_resource*"),
            "panic must point to acquire_resource"
        );
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_no_asset_root_with_absolute_key() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        fs::write(&file_path, b"test data").unwrap();

        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .build();

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"test data");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_no_asset_root_rejects_relative_key() {
        let dir = tempdir().unwrap();

        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .build();

        let key = ResourceKey::new("test.bin");
        let result = store.open_resource(&key);
        assert!(result.is_err());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn build_ephemeral_returns_mem() {
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .ephemeral(true)
            .build();
        assert!(backend.is_ephemeral());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn ephemeral_capabilities_lack_evict_and_lease() {
        let store = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .build_ephemeral();
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
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test"))
            .build_disk();
        assert_eq!(store.capabilities(), Capabilities::all());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn disk_local_mode_only_processing() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .build_disk();
        assert_eq!(store.capabilities(), Capabilities::PROCESSING);
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn build_disk_returns_disk() {
        let dir = tempdir().unwrap();
        let backend = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test"))
            .ephemeral(false)
            .build();
        assert!(!backend.is_ephemeral());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn ephemeral_retains_data_within_cache_capacity() {
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .cache_capacity(NonZeroUsize::new(5).unwrap())
            .ephemeral(true)
            .build();

        // Open 4 resources — all fit within cache capacity of 5.
        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            let res = backend.acquire_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // All resources are still in the LRU — re-opening returns the same handle.
        let reopened = backend.open_resource(&keys[0]).unwrap();
        assert_eq!(
            reopened.len(),
            Some(4),
            "resource within cache capacity must retain data"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn ephemeral_evicts_data_beyond_cache_capacity() {
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .cache_capacity(NonZeroUsize::new(3).unwrap())
            .ephemeral(true)
            .build();

        // Open 4 resources — first is evicted from LRU (capacity=3).
        // MemAssetStore is stateless, so evicted data is gone.
        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            let res = backend.acquire_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // First resource was evicted from LRU — re-opening now reports Missing.
        assert!(
            backend.open_resource(&keys[0]).is_err(),
            "evicted resource should be gone in ephemeral mode"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn from_asset_store() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test"))
            .build_disk();
        let backend: AssetStore = store.into();
        assert!(!backend.is_ephemeral());
    }

    /// Red test pinning the root cause of the
    /// `local_queue_playlist_behavior_*` HLS+AES128 hang
    /// (`kithara_stream::stream::try_read` spins until the 10 s hang
    /// detector panics).
    ///
    /// The exact production trace is:
    ///
    /// ```text
    /// LeaseResource::drop -> remove_resource (BYPASSES availability invalidation)
    ///     key=Relative("v0_15.m4s") status=Active
    /// DiskStore::remove_resource
    ///     existed=true availability_final_len=65640
    /// ...
    /// DiskStore::open_resource: NotFound (file missing)
    ///     availability_final_len=65640 availability_contains_0_1=true
    /// ```
    ///
    /// `AvailabilityIndex` is the canonical reflection of what is on
    /// disk. Once a `commit` observer records `final_len`, the index
    /// is the source of truth and must be invalidated synchronously
    /// with any disk-side deletion. `CachedAssets` does NOT cause this
    /// divergence — it only caches in-memory handles. The bug is in
    /// the disk-side deletion path skipping availability invalidation.
    ///
    /// Path doing the divergent deletion: `LeaseResource::drop`
    /// (lease.rs:367-394). When the writer drops with
    /// `status = Active | Failed(_)` and `drop_token` strong count
    /// reaches one, it runs the `RemoveFn` closure (lease.rs:484-488)
    /// which does `inner.remove_resource(key)` — descending straight
    /// through `CachedAssets`/`ProcessingAssets`/`EvictAssets` to
    /// `DiskAssetStore::remove_resource` and `fs::remove_file`.
    /// `unified::AssetStore::remove_resource` (unified.rs:152) — the
    /// only place that calls `availability.remove` next to
    /// `store.remove_resource` — is bypassed entirely.
    ///
    /// This reproduction triggers the exact production drop path:
    /// commit a writer, then `reactivate()` the resource (the live
    /// path that flips `MmapState::Committed → Active`, mmap.rs:289)
    /// and drop without commit. `LeaseResource::drop` then sees
    /// `status == Active` and runs `RemoveFn`. After the drop the
    /// file is gone, but `AvailabilityIndex` still holds the
    /// committed `final_len`. Same divergence as the production
    /// trace, no LRU manipulation, no manual `fs` calls.
    ///
    /// FAILING today on the second assertion. Will pass once the
    /// disk-side deletion path syncs `AvailabilityIndex`.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn red_test_lease_resource_drop_strands_availability_index() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("seg_root"))
            .build();

        let target = ResourceKey::new("v0_15.m4s");

        // Phase 1 — committed writer. AvailabilityIndex receives
        // `final_len = 4` via the MmapDriver commit observer.
        {
            let writer = store.acquire_resource(&target).unwrap();
            writer.write_at(0, b"data").unwrap();
            writer.commit(Some(4)).unwrap();
        }
        assert!(store.contains_range(&target, 0..4));
        let path = dir.path().join("seg_root").join("v0_15.m4s");
        assert!(path.exists(), "file must exist after commit");

        // Phase 2 — exact production drop path. Acquire the resource
        // again (cache returns the committed clone), reactivate it
        // (MmapDriver: Committed → Active, mmap.rs:289-305), then
        // drop without a fresh commit. `LeaseResource::drop` sees
        // `status == Active`, drop_token strong count == 1, and runs
        // `RemoveFn` (lease.rs:391) → `inner.remove_resource(key)` →
        // DiskStore deletes the file. Availability is never told.
        {
            let writer2 = store.acquire_resource(&target).unwrap();
            writer2.reactivate().expect("reactivate committed");
        }

        // Post-condition: file is gone from disk, but the index never
        // received `availability.remove`.
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

    /// Red test pinning the SECOND bypass surfaced by the parallel
    /// `local_queue_playlist_behavior_symphonia` failure in
    /// `just test`. After fixing `LeaseResource::drop`'s availability
    /// invalidation (`lease.rs` → `DiskStore::remove_resource`), the same
    /// HLS+AES128 hang still reproduces under parallel pressure.
    ///
    /// Trace: any caller that deletes an entire `asset_root` directory
    /// (instead of a single resource) goes through `delete_asset_dir`
    /// (`fs::remove_dir_all`) without telling `AvailabilityIndex`.
    /// Three production paths take this shortcut:
    ///
    /// 1. `DiskAssetStore::delete_asset` (in `disk_store.rs`) — invoked
    ///    via `AssetStore::delete_asset` and `CachedAssets::delete_asset`.
    /// 2. `EvictAssets::on_asset_created` (in `evict.rs`) — LRU
    ///    eviction triggered when a new `asset_root` is observed.
    /// 3. `EvictAssets::evict_one` → `delete_and_forget`
    ///    (evict.rs:219) — explicit byte-cap eviction.
    ///
    /// All three invalidate the directory but leave per-resource
    /// entries stranded in `AvailabilityIndex`. Subsequent
    /// `contains_range` / `final_len` queries hit the hot-path map and
    /// answer with stale `true` / `Some(len)`. A `wait_range` caller
    /// then issues a `read_at` that returns `NotFound` — same race as
    /// the first bypass, observable as a 10 s `hang_detector` panic.
    ///
    /// The minimal repro below uses `AssetStore::delete_asset` because
    /// it is the most direct of the three. Once
    /// `AvailabilityIndex::clear_root` (or equivalent) is wired into
    /// every directory-deletion path, this test passes and the
    /// remaining production hang lifts.
    ///
    /// FAILING today on the second assertion.
    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn red_test_delete_asset_strands_availability_index() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("seg_root"))
            .build();

        let key_a = ResourceKey::new("v0_15.m4s");
        let key_b = ResourceKey::new("v0_16.m4s");

        // Commit two resources under the same asset_root. Availability
        // observer records `final_len` for both via MmapDriver::commit.
        for (key, payload) in [(&key_a, &b"aaaa"[..]), (&key_b, &b"bbbbb"[..])] {
            let writer = store.acquire_resource(key).unwrap();
            writer.write_at(0, payload).unwrap();
            writer.commit(Some(payload.len() as u64)).unwrap();
        }
        assert!(store.contains_range(&key_a, 0..4));
        assert!(store.contains_range(&key_b, 0..5));

        let path_a = dir.path().join("seg_root").join("v0_15.m4s");
        let path_b = dir.path().join("seg_root").join("v0_16.m4s");
        assert!(path_a.exists());
        assert!(path_b.exists());

        // `AssetStore::delete_asset` removes the entire `asset_root`
        // directory through `delete_asset_dir` (`fs::remove_dir_all`).
        // Files under it disappear, but the per-resource availability
        // map under `asset_root` is never cleared.
        store.delete_asset().unwrap();

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
