#![forbid(unsafe_code)]

use std::{hash::Hash, num::NonZeroUsize, path::PathBuf, sync::Arc};

use derive_setters::Setters;
use kithara_bufpool::{BytePool, byte_pool};
use kithara_storage::StorageResource;
use tokio_util::sync::CancellationToken;

/// Default in-memory LRU cache capacity (enough for init + 2-3 media segments).
const DEFAULT_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(5).unwrap();

#[cfg(not(target_arch = "wasm32"))]
use crate::disk_store::DiskAssetStore;
use crate::{
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
    mem_store::MemAssetStore,
    process::{ProcessChunkFn, ProcessedResource, ProcessingAssets},
    unified::AssetStore,
};

/// Simplified storage options for creating an asset store.
///
/// Used by higher-level crates (kithara-file, kithara-hls) for unified configuration.
/// This provides a user-friendly API that hides internal details like `asset_root`.
#[derive(Clone, Debug, Setters)]
#[setters(prefix = "with_", strip_option)]
pub struct StoreOptions {
    /// Directory for persistent cache storage (required).
    pub cache_dir: PathBuf,
    /// In-memory LRU cache capacity for opened resources.
    pub cache_capacity: Option<NonZeroUsize>,
    /// Use ephemeral (in-memory) storage instead of disk.
    ///
    /// When `true`, the asset store uses `MemAssetStore` instead of
    /// `DiskAssetStore`. Data is never written to disk.
    /// Default: `false`.
    pub ephemeral: bool,
    /// Maximum number of assets to keep (soft cap for LRU eviction).
    pub max_assets: Option<usize>,
    /// Maximum bytes to store (soft cap for LRU eviction).
    pub max_bytes: Option<u64>,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            cache_dir: std::env::temp_dir().join("kithara"),
            #[cfg(target_arch = "wasm32")]
            cache_dir: PathBuf::from("/kithara"),
            cache_capacity: None,
            ephemeral: false,
            max_assets: None,
            max_bytes: None,
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
            max_assets: None,
            max_bytes: None,
        }
    }

    /// Effective LRU cache capacity (explicit or default).
    #[must_use]
    pub fn effective_cache_capacity(&self) -> NonZeroUsize {
        self.cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY)
    }

    /// Convert to internal `EvictConfig`.
    #[must_use]
    pub fn to_evict_config(&self) -> EvictConfig {
        EvictConfig {
            max_assets: self.max_assets,
            max_bytes: self.max_bytes,
        }
    }
}

/// Fully decorated asset store with processing layer.
///
/// ## Decorator order (inside to outside)
/// - `DiskAssetStore` (base disk I/O)
/// - `EvictAssets` (LRU eviction)
/// - `ProcessingAssets` (transformation with context, uses Default if no context)
/// - `LeaseAssets` (RAII pinning)
/// - `CachedAssets` (caches `LeaseResource` with guards, outermost)
///
/// Generic parameter `Ctx` is the context type for processing.
/// Use `()` (default) for no processing (`ProcessingAssets` will pass through unchanged).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type DiskStore<Ctx = ()> =
    CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<DiskAssetStore>, Ctx>>>;

/// Resource handle returned by [`AssetStore::open_resource`].
///
/// Wraps `StorageResource` with processing and lease semantics.
/// Implements `ResourceExt` for read/write/commit operations.
/// Both disk and memory variants return this same type.
pub type AssetResource<Ctx = ()> =
    LeaseResource<ProcessedResource<StorageResource, Ctx>, LeaseGuard>;

/// In-memory asset store with disabled decorators.
///
/// Internal chain used for `AssetStore::Mem`.
pub(crate) type MemStore<Ctx = ()> =
    CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<MemAssetStore>, Ctx>>>;

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
    cache_capacity: Option<NonZeroUsize>,
    cancel: Option<CancellationToken>,
    ephemeral: bool,
    evict_config: Option<EvictConfig>,
    mem_resource_capacity: Option<usize>,
    pool: Option<BytePool>,
    process_fn: Option<ProcessChunkFn<Ctx>>,
    root_dir: Option<PathBuf>,
    asset_root: Option<String>,
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
            mem_resource_capacity: None,
            pool: None,
            process_fn: Some(dummy_process),
            root_dir: None,
            asset_root: None,
        }
    }
}

impl<Ctx> AssetStoreBuilder<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + std::fmt::Debug + 'static,
{
    /// Build the storage backend.
    ///
    /// Returns `AssetStore::Disk` for persistent storage or
    /// `AssetStore::Mem` when `ephemeral(true)` is set.
    ///
    /// # Panics
    /// Panics if `process_fn` is not set.
    #[must_use]
    pub fn build(self) -> AssetStore<Ctx> {
        #[cfg(target_arch = "wasm32")]
        {
            self.build_ephemeral().into()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.ephemeral {
                self.build_ephemeral().into()
            } else {
                self.build_disk().into()
            }
        }
    }

    /// Set the root directory for the asset store.
    pub fn root_dir<P: Into<PathBuf>>(mut self, root: P) -> Self {
        self.root_dir = Some(root.into());
        self
    }

    /// Set the asset root identifier (e.g. from `asset_root_for_url`).
    ///
    /// Pass `None` when the store will only be used with absolute keys
    /// (e.g. local file playback). Relative keys will fail with `InvalidKey`
    /// when `asset_root` is `None`.
    pub fn asset_root(mut self, asset_root: Option<&str>) -> Self {
        self.asset_root = asset_root.map(str::to_string);
        self
    }

    #[must_use]
    pub fn evict_config(mut self, cfg: EvictConfig) -> Self {
        self.evict_config = Some(cfg);
        self
    }

    #[must_use]
    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set capacity of each in-memory resource for ephemeral backend.
    #[must_use]
    pub fn mem_resource_capacity(mut self, capacity: usize) -> Self {
        self.mem_resource_capacity = Some(capacity);
        self
    }

    /// Set the in-memory LRU cache capacity for opened resources.
    #[must_use]
    pub fn cache_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.cache_capacity = Some(capacity);
        self
    }

    /// Set the buffer pool (created at application startup and shared).
    #[must_use]
    pub fn pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
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

    /// Build disk-backed asset store.
    ///
    /// # Panics
    /// Panics if `process_fn` is not set for Ctx != ().
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn build_disk(self) -> DiskStore<Ctx> {
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

        // Use provided pool or global pool
        let pool = self.pool.unwrap_or_else(|| byte_pool().clone());

        // Build decorator chain: Disk -> Evict -> Processing -> Lease -> Cached
        // Each decorator checks `capabilities()` to decide whether to activate.
        let disk = Arc::new(DiskAssetStore::new(root_dir, asset_root, cancel.clone()));
        let evict = Arc::new(EvictAssets::new(
            disk,
            evict_cfg,
            cancel.clone(),
            pool.clone(),
        ));
        let processing = Arc::new(ProcessingAssets::new(
            Arc::clone(&evict),
            process_fn,
            pool.clone(),
        ));
        let byte_recorder: Option<Arc<dyn crate::evict::ByteRecorder>> =
            Some(Arc::clone(&evict) as Arc<dyn crate::evict::ByteRecorder>);
        let lease = LeaseAssets::with_byte_recorder(processing, cancel, byte_recorder, pool);
        let capacity = self.cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY);
        CachedAssets::new(Arc::new(lease), capacity)
    }

    /// Build ephemeral (in-memory) asset store.
    ///
    /// `MemAssetStore` is a stateless factory — each `open_resource` creates a
    /// fresh `MemResource`. The `CachedAssets` LRU is the single owner: when a
    /// handle is evicted its `Arc` ref-count drops and memory is freed.
    fn build_ephemeral(self) -> MemStore<Ctx> {
        let asset_root = self.asset_root.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();
        let evict_cfg = self.evict_config.unwrap_or_default();
        let process_fn = self
            .process_fn
            .expect("process_fn is required for AssetStoreBuilder");
        let pool = self.pool.unwrap_or_else(|| byte_pool().clone());

        let mem = Arc::new(MemAssetStore::new(
            asset_root,
            cancel.clone(),
            self.mem_resource_capacity,
        ));
        let evict = Arc::new(EvictAssets::new(
            mem,
            evict_cfg,
            cancel.clone(),
            pool.clone(),
        ));
        let processing = Arc::new(ProcessingAssets::new(
            Arc::clone(&evict),
            process_fn,
            pool.clone(),
        ));
        let lease = LeaseAssets::new(processing, cancel, pool);
        let capacity = self.cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY);
        CachedAssets::new(Arc::new(lease), capacity)
    }
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
            mem_resource_capacity: self.mem_resource_capacity,
            pool: self.pool,
            process_fn: Some(f),
            root_dir: self.root_dir,
            asset_root: self.asset_root,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_storage::ResourceExt;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::{base::Assets, key::ResourceKey};

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_local_mode_decorators_inactive() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.bin");
        std::fs::write(&file_path, b"data").unwrap();

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
        let res = store.open_resource(&key).unwrap();
        res.write_at(0, b"hello").unwrap();

        let mut buf = [0u8; 5];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn builder_no_asset_root_with_absolute_key() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        std::fs::write(&file_path, b"test data").unwrap();

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
        use crate::base::Capabilities;
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
        use crate::base::Capabilities;
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
        use crate::base::Capabilities;
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
            let res = backend.open_resource(key).unwrap();
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
            let res = backend.open_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // First resource was evicted from LRU — re-opening creates a fresh empty one.
        let reopened = backend.open_resource(&keys[0]).unwrap();
        assert_eq!(
            reopened.len(),
            None,
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
}
