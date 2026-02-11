#![forbid(unsafe_code)]

use std::{hash::Hash, num::NonZeroUsize, path::PathBuf, sync::Arc};

use kithara_bufpool::{BytePool, byte_pool};
use kithara_storage::StorageResource;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

/// Default in-memory LRU cache capacity (enough for init + 2-3 media segments).
const DEFAULT_CACHE_CAPACITY: NonZeroUsize = match NonZeroUsize::new(5) {
    Some(v) => v,
    None => unreachable!(),
};

use crate::{
    backend::AssetsBackend,
    base::DiskAssetStore,
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
    mem_store::MemAssetStore,
    process::{ProcessChunkFn, ProcessedResource, ProcessingAssets},
};

/// Simplified storage options for creating an asset store.
///
/// Used by higher-level crates (kithara-file, kithara-hls) for unified configuration.
/// This provides a user-friendly API that hides internal details like `asset_root`.
#[derive(Clone, Debug)]
pub struct StoreOptions {
    /// Directory for persistent cache storage (required).
    pub cache_dir: PathBuf,
    /// In-memory LRU cache capacity for opened resources.
    pub cache_capacity: Option<NonZeroUsize>,
    /// Maximum number of assets to keep (soft cap for LRU eviction).
    pub max_assets: Option<usize>,
    /// Maximum bytes to store (soft cap for LRU eviction).
    pub max_bytes: Option<u64>,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            cache_dir: std::env::temp_dir().join("kithara"),
            cache_capacity: None,
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
            max_assets: None,
            max_bytes: None,
        }
    }

    /// Set maximum number of assets to keep.
    pub fn with_max_assets(mut self, max: usize) -> Self {
        self.max_assets = Some(max);
        self
    }

    /// Set maximum bytes to store.
    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = Some(max);
        self
    }

    /// Convert to internal `EvictConfig`.
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
pub type AssetStore<Ctx = ()> =
    CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<DiskAssetStore>, Ctx>>>;

/// Resource handle returned by [`AssetStore::open_resource`] or [`MemStore::open_resource`].
///
/// Wraps `StorageResource` with processing and lease semantics.
/// Implements `ResourceExt` for read/write/commit operations.
/// Both `AssetStore` (disk) and `MemStore` (memory) return this same type.
pub type AssetResource<Ctx = ()> =
    LeaseResource<ProcessedResource<StorageResource, Ctx>, LeaseGuard>;

/// In-memory asset store with disabled decorators.
///
/// Same decorator chain as [`AssetStore`] but backed by [`MemAssetStore`]
/// instead of [`DiskAssetStore`]. All decorators run in disabled/no-op mode.
/// Returns the same [`AssetResource`] type as `AssetStore`.
pub type MemStore<Ctx = ()> =
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
    cache_enabled: bool,
    cancel: Option<CancellationToken>,
    ephemeral: bool,
    evict_config: Option<EvictConfig>,
    evict_enabled: bool,
    lease_enabled: bool,
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
    pub fn new() -> Self {
        // Default pass-through process_fn for () - just copies input to output
        let dummy_process: ProcessChunkFn<()> = Arc::new(|input, output, _ctx, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });

        Self {
            cache_capacity: None,
            cache_enabled: true,
            cancel: None,
            ephemeral: false,
            evict_config: None,
            evict_enabled: true,
            lease_enabled: true,
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
    /// Returns `AssetsBackend::Disk` for persistent storage or
    /// `AssetsBackend::Mem` when `ephemeral(true)` is set.
    ///
    /// # Panics
    /// Panics if `process_fn` is not set.
    pub fn build(self) -> AssetsBackend<Ctx> {
        if self.ephemeral {
            self.build_ephemeral().into()
        } else {
            self.build_disk().into()
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

    pub fn evict_config(mut self, cfg: EvictConfig) -> Self {
        self.evict_config = Some(cfg);
        self
    }

    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set the in-memory LRU cache capacity for opened resources.
    pub fn cache_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.cache_capacity = Some(capacity);
        self
    }

    /// Set the buffer pool (created at application startup and shared).
    pub fn pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Enable or disable the in-memory LRU cache layer.
    ///
    /// When disabled, `CachedAssets` delegates directly to the inner layer.
    /// Default: `true`.
    pub fn cache_enabled(mut self, enabled: bool) -> Self {
        self.cache_enabled = enabled;
        self
    }

    /// Enable or disable the eviction layer.
    ///
    /// When disabled, `EvictAssets` delegates directly to the inner layer
    /// (no LRU tracking, no eviction).
    /// Default: `true`.
    pub fn evict_enabled(mut self, enabled: bool) -> Self {
        self.evict_enabled = enabled;
        self
    }

    /// Enable or disable the lease (pin) layer.
    ///
    /// When disabled, `LeaseAssets` delegates directly to the inner layer
    /// (no pinning, no byte recording, no persistence).
    /// Default: `true`.
    pub fn lease_enabled(mut self, enabled: bool) -> Self {
        self.lease_enabled = enabled;
        self
    }

    /// Use ephemeral (in-memory) storage instead of disk.
    ///
    /// When `true`, `build()` returns `AssetsBackend::Mem` with auto-eviction
    /// (LRU cache removes underlying data on eviction).
    /// Default: `false`.
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// Build disk-backed asset store.
    ///
    /// # Panics
    /// Panics if `process_fn` is not set for Ctx != ().
    pub fn build_disk(self) -> AssetStore<Ctx> {
        let root_dir = self.root_dir.unwrap_or_else(|| {
            tempdir()
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
        let disk = Arc::new(DiskAssetStore::new(root_dir, asset_root, cancel.clone()));
        let evict = Arc::new(EvictAssets::new(
            disk,
            evict_cfg,
            cancel.clone(),
            self.evict_enabled,
        ));
        let processing = Arc::new(ProcessingAssets::new(evict.clone(), process_fn, pool));

        // LeaseAssets holds evict for byte recording
        let byte_recorder: Option<Arc<dyn crate::evict::ByteRecorder>> = if self.lease_enabled {
            Some(evict as Arc<dyn crate::evict::ByteRecorder>)
        } else {
            None
        };
        let lease =
            LeaseAssets::with_options(processing, cancel, byte_recorder, self.lease_enabled);

        // CachedAssets on top to cache LeaseResource with guards
        let capacity = self.cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY);
        CachedAssets::with_options(Arc::new(lease), capacity, self.cache_enabled, false)
    }

    /// Build ephemeral (in-memory) asset store with auto-eviction.
    fn build_ephemeral(self) -> MemStore<Ctx> {
        let root_dir = self
            .root_dir
            .unwrap_or_else(|| std::env::temp_dir().join("kithara-ephemeral"));
        let asset_root = self.asset_root.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();
        let process_fn = self
            .process_fn
            .expect("process_fn is required for AssetStoreBuilder");
        let pool = self.pool.unwrap_or_else(|| byte_pool().clone());

        let mem = Arc::new(MemAssetStore::new(asset_root, cancel.clone(), root_dir));
        let evict = Arc::new(EvictAssets::new(
            mem,
            EvictConfig::default(),
            cancel.clone(),
            false,
        ));
        let processing = Arc::new(ProcessingAssets::new(evict, process_fn, pool));
        let lease = LeaseAssets::with_options(processing, cancel, None, false);
        let capacity = self.cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY);
        CachedAssets::with_options(Arc::new(lease), capacity, true, true)
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
            cache_enabled: self.cache_enabled,
            cancel: self.cancel,
            ephemeral: self.ephemeral,
            evict_config: self.evict_config,
            evict_enabled: self.evict_enabled,
            lease_enabled: self.lease_enabled,
            pool: self.pool,
            process_fn: Some(f),
            root_dir: self.root_dir,
            asset_root: self.asset_root,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_storage::ResourceExt;
    use rstest::rstest;

    use super::*;
    use crate::key::ResourceKey;

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn builder_all_decorators_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test_asset"))
            .cache_enabled(false)
            .lease_enabled(false)
            .evict_enabled(false)
            .build();

        let key = ResourceKey::new("test.bin");
        let res = store.open_resource(&key).unwrap();

        // Resource should be fully functional
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        let mut buf = [0u8; 4];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn builder_defaults_all_enabled() {
        let dir = tempfile::tempdir().unwrap();
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

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn builder_no_asset_root_with_absolute_key() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("song.mp3");
        std::fs::write(&file_path, b"test data").unwrap();

        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .cache_enabled(false)
            .lease_enabled(false)
            .evict_enabled(false)
            .build();

        let key = ResourceKey::absolute(&file_path);
        let res = store.open_resource(&key).unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"test data");
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn builder_no_asset_root_rejects_relative_key() {
        let dir = tempfile::tempdir().unwrap();

        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(None)
            .cache_enabled(false)
            .lease_enabled(false)
            .evict_enabled(false)
            .build();

        let key = ResourceKey::new("test.bin");
        let result = store.open_resource(&key);
        assert!(result.is_err());
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn build_ephemeral_returns_mem() {
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .ephemeral(true)
            .build();
        assert!(backend.is_ephemeral());
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn build_disk_returns_disk() {
        let dir = tempfile::tempdir().unwrap();
        let backend = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test"))
            .ephemeral(false)
            .build();
        assert!(!backend.is_ephemeral());
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn ephemeral_auto_evicts_on_capacity() {
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .cache_capacity(NonZeroUsize::new(3).unwrap())
            .ephemeral(true)
            .build();

        // Open 4 resources — first should be auto-evicted
        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            let res = backend.open_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // First resource should have been evicted (removed from MemStore).
        // Re-opening gives a fresh empty resource.
        let reopened = backend.open_resource(&keys[0]).unwrap();
        assert_eq!(reopened.len(), None, "evicted resource should be gone");
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn from_asset_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("test"))
            .build_disk();
        let backend: AssetsBackend = store.into();
        assert!(!backend.is_ephemeral());
    }
}
