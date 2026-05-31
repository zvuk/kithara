#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::env;
use std::{fmt, hash::Hash, num::NonZeroUsize, path::PathBuf, sync::Arc};

use bon::Builder;
use dashmap::DashMap;
use kithara_bufpool::BytePool;
use tokio_util::sync::CancellationToken;

#[cfg(not(target_arch = "wasm32"))]
use crate::disk_store::DiskAssetStore;
use crate::{
    acquisition::AcquisitionResult,
    base::{BaseReader, BaseWriter},
    cache::{CachedAssets, CachedReader, CachedWriter},
    evict::EvictAssets,
    flush::{FlushHub, FlushPolicy},
    index::{AvailabilityIndex, DemandIndex, EvictConfig},
    key::ResourceKey,
    lease::{LeaseAssets, LeaseGuard, LeaseReader, LeaseWriter},
    mem_store::MemAssetStore,
    process::{ProcessChunkFn, ProcessedReader, ProcessedWriter, ProcessingAssets},
    unified::AssetStore,
};

/// Private module-level defaults, grouped per ast-grep style rule.
struct Consts;
impl Consts {
    /// Default in-memory LRU cache capacity (init + 2-3 media segments).
    const DEFAULT_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();
}

/// Callback invoked when a cached resource is invalidated (displaced from LRU cache).
///
/// In ephemeral mode this means data loss (no disk backing).
/// In disk mode the data may still be on disk but the handle is gone.
pub type OnInvalidatedFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

/// Simplified storage options for creating an asset store.
///
/// Used by higher-level crates (kithara-file, kithara-hls) for unified configuration.
/// This provides a user-friendly API that hides internal details like `asset_root`.
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
    /// Maximum number of assets to keep (soft cap for LRU eviction).
    pub max_assets: Option<usize>,
    /// Maximum bytes to store (soft cap for LRU eviction).
    pub max_bytes: Option<u64>,
    /// Called when a cached resource is invalidated (displaced from LRU cache).
    pub on_invalidated: Option<OnInvalidatedFn>,
    /// Directory for persistent cache storage (required).
    pub cache_dir: PathBuf,
    /// Use ephemeral (in-memory) storage instead of disk.
    ///
    /// When `true`, the asset store uses `MemAssetStore` instead of
    /// `DiskAssetStore`. Data is never written to disk.
    /// Default: `false`.
    #[builder(default)]
    pub is_ephemeral: bool,
}

impl fmt::Debug for StoreOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreOptions")
            .field("cache_dir", &self.cache_dir)
            .field("cache_capacity", &self.cache_capacity)
            .field("is_ephemeral", &self.is_ephemeral)
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
        Self::default_builder().build()
    }
}

impl StoreOptions {
    /// Create options with `cache_dir` set and all other fields at their builder defaults.
    pub fn new<P>(cache_dir: P) -> Self
    where
        P: Into<PathBuf>,
    {
        Self::builder().cache_dir(cache_dir.into()).build()
    }

    /// Builder pre-populated with the platform default `cache_dir`.
    ///
    /// Allows `StoreOptions::default_builder().is_ephemeral(true).build()` —
    /// the chainable counterpart to `StoreOptions::default()`.
    pub fn default_builder() -> StoreOptionsBuilder<store_options_builder::SetCacheDir> {
        #[cfg(not(target_arch = "wasm32"))]
        let cache_dir = env::temp_dir().join("kithara");
        #[cfg(target_arch = "wasm32")]
        let cache_dir = PathBuf::from("/kithara");
        Self::builder().cache_dir(cache_dir)
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

/// Pending (writer) handle returned by the `Pending` arm of
/// [`AssetStore::acquire_resource`]. Owns the streaming write + decrypt-on-commit
/// capability; consumes itself on `commit` into an [`AssetReader`].
pub type AssetWriter<Ctx = ()> =
    LeaseWriter<CachedWriter<ProcessedWriter<BaseWriter, Ctx>>, LeaseGuard>;

/// Ready (reader) handle returned by [`AssetStore::open_resource`] and the
/// `Ready` arm of [`AssetStore::acquire_resource`]. Cheap to clone.
pub type AssetReader<Ctx = ()> =
    LeaseReader<CachedReader<ProcessedReader<BaseReader, Ctx>>, LeaseGuard>;

/// Phase-typed acquisition outcome returned by
/// [`AssetStore::acquire_resource`]: a `Pending` [`AssetWriter`] to stream and
/// commit, or a `Ready` [`AssetReader`] when the resource is already committed.
pub type AssetResource<Ctx = ()> = AcquisitionResult<AssetWriter<Ctx>, AssetReader<Ctx>>;

/// In-memory asset store with disabled decorators.
///
/// Internal chain used for `AssetStore::Mem`.
pub(crate) type MemStore<Ctx = ()> =
    LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<MemAssetStore>, Ctx>>>;

/// Constructor for the ready-to-use [`AssetStore`].
///
/// ## Usage
///
/// One store services every asset under `root_dir`. A scope binds the
/// `asset_root` and mints self-contained keys; per-resource ops live on
/// the store.
/// ```ignore
/// // Without processing:
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .build();
/// let scope = store.scope(asset_root_for_url(&url, None));
/// let res = store.acquire_resource(&scope.key_from_url(&url), None)?;
///
/// // With processing callback:
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .process_fn(my_decrypt_callback)
///     .build();
/// ```
///
/// ## Decorator order (normative)
/// - `EvictAssets` is applied first (evaluates eviction at "asset creation time")
/// - `CachedAssets` caches opened resources in memory
/// - `LeaseAssets` provides RAII pinning for opened resources (outermost)
/// - `ProcessingAssets` (if configured) wraps resources for transformation
pub struct AssetStoreBuilder<Ctx: Clone + Hash + Eq + Send + Sync + 'static = ()> {
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
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
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
        }
    }
}

impl<Ctx> AssetStoreBuilder<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + fmt::Debug + 'static,
{
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
        // The demand index is a consumer-driven sibling of `availability`:
        // no observer / decorator threading, just a shared field. Each
        // slot's `producer_cancel` is a child of this store cancel.
        let demand = DemandIndex::new(self.cancel.clone().unwrap_or_default());
        #[cfg(target_arch = "wasm32")]
        {
            let store = self.build_ephemeral_with_availability(&availability);
            AssetStore::Mem {
                store,
                availability,
                demand,
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.ephemeral {
                let store = self.build_ephemeral_with_availability(&availability);
                AssetStore::Mem {
                    store,
                    availability,
                    demand,
                }
            } else {
                let (store, base) = self.build_disk_with_availability(availability.clone());
                AssetStore::Disk {
                    store,
                    availability,
                    demand,
                    base: Some(base),
                }
            }
        }
    }

    /// Build a disk-backed `AssetStore` chain with a fresh availability index.
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
                .expect("BUG: failed to create AssetStore temp dir")
                .keep()
        });
        let evict_cfg = self.evict_config.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();

        let process_fn = self
            .process_fn
            .expect("BUG: process_fn is required for AssetStoreBuilder");

        let pool = self.pool.unwrap_or_else(|| BytePool::default().clone());

        let hub = self
            .flush_hub
            .clone()
            .unwrap_or_else(|| FlushHub::new(cancel.child_token(), FlushPolicy::default()));

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
        let cached = Arc::new(CachedAssets::new(
            processing,
            capacity,
            self.on_invalidated,
            false,
        ));
        let byte_recorder: Option<Arc<dyn crate::evict::ByteRecorder>> =
            Some(Arc::clone(&evict) as Arc<dyn crate::evict::ByteRecorder>);
        let _ = pool;
        let chain = LeaseAssets::with_byte_recorder(cached, cancel, byte_recorder, pins);
        (chain, base)
    }

    /// Build ephemeral (in-memory) asset store with its own
    /// unshared [`AvailabilityIndex`].
    #[cfg(test)]
    fn build_ephemeral(self) -> MemStore<Ctx> {
        self.build_ephemeral_with_availability(&AvailabilityIndex::new())
    }

    fn build_ephemeral_with_availability(self, availability: &AvailabilityIndex) -> MemStore<Ctx> {
        let cancel = self.cancel.unwrap_or_default();
        let evict_cfg = self.evict_config.unwrap_or_default();
        let process_fn = self
            .process_fn
            .expect("BUG: process_fn is required for AssetStoreBuilder");
        let pool = self.pool.unwrap_or_else(|| BytePool::default().clone());

        let hub = self
            .flush_hub
            .clone()
            .unwrap_or_else(|| FlushHub::new(cancel.child_token(), FlushPolicy::default()));
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
        let availability_for_hook = availability.clone();
        let hooked_on_invalidated: OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            availability_for_hook.remove(key);
            if let Some(ref cb) = user_on_invalidated {
                cb(key);
            }
        });
        let cached = Arc::new(CachedAssets::new(
            processing,
            capacity,
            Some(hooked_on_invalidated),
            true,
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
    /// See [`StoreOptions`] builder's `flush_hub` setter.
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
        }
    }
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

        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();

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
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();

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

        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key, None).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"test data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn build_ephemeral_returns_mem() {
        let backend = AssetStoreBuilder::new().ephemeral(true).build();
        assert!(backend.is_ephemeral());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn ephemeral_capabilities_lack_evict_and_lease() {
        let store = AssetStoreBuilder::new().build_ephemeral();
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
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build_disk();
        assert_eq!(store.capabilities(), Capabilities::all());
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn build_disk_returns_disk() {
        let dir = tempdir().unwrap();
        let backend = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .ephemeral(false)
            .build();
        assert!(matches!(backend, AssetStore::Disk { .. }));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn ephemeral_retains_data_within_cache_capacity() {
        let backend = AssetStoreBuilder::new()
            .cache_capacity(NonZeroUsize::new(5).unwrap())
            .ephemeral(true)
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
    fn ephemeral_evicts_data_beyond_cache_capacity() {
        let backend = AssetStoreBuilder::new()
            .cache_capacity(NonZeroUsize::new(3).unwrap())
            .ephemeral(true)
            .build();

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            write_commit(backend.acquire_resource(key, None).unwrap(), b"data");
        }

        assert!(
            backend.open_resource(&keys[0], None).is_err(),
            "evicted resource should be gone in ephemeral mode"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn from_asset_store() {
        let dir = tempdir().unwrap();
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build_disk();
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
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
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
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
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
