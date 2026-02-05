#![forbid(unsafe_code)]

use std::{hash::Hash, path::PathBuf, sync::Arc};

use kithara_bufpool::{BytePool, byte_pool};
use kithara_storage::StorageResource;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crate::{
    base::DiskAssetStore,
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    lease::{LeaseAssets, LeaseGuard, LeaseResource},
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
    /// Maximum number of assets to keep (soft cap for LRU eviction).
    pub max_assets: Option<usize>,
    /// Maximum bytes to store (soft cap for LRU eviction).
    pub max_bytes: Option<u64>,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self::new(std::env::temp_dir().join("kithara"))
    }
}

impl StoreOptions {
    /// Create new store options with the given cache directory.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
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

    /// Convert to internal EvictConfig.
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
/// - DiskAssetStore (base disk I/O)
/// - EvictAssets (LRU eviction)
/// - ProcessingAssets (transformation with context, uses Default if no context)
/// - LeaseAssets (RAII pinning)
/// - CachedAssets (caches LeaseResource with guards, outermost)
///
/// Generic parameter `Ctx` is the context type for processing.
/// Use `()` (default) for no processing (ProcessingAssets will pass through unchanged).
pub type AssetStore<Ctx = ()> =
    CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<DiskAssetStore>, Ctx>>>;

/// Resource handle returned by [`AssetStore::open_resource`].
///
/// Wraps `StorageResource` with processing and lease semantics.
/// Implements `ResourceExt` for read/write/commit operations.
pub type AssetResource<Ctx = ()> = LeaseResource<
    ProcessedResource<StorageResource, Ctx>,
    LeaseGuard<ProcessingAssets<EvictAssets<DiskAssetStore>, Ctx>>,
>;

/// Constructor for the ready-to-use [`AssetStore`].
///
/// ## Usage
/// ```ignore
/// // Without processing:
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .asset_root(asset_root_for_url(&master_url))
///     .build();
///
/// // With processing callback:
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .asset_root(asset_root_for_url(&master_url))
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
    root_dir: Option<PathBuf>,
    asset_root: Option<String>,
    evict_config: Option<EvictConfig>,
    cancel: Option<CancellationToken>,
    process_fn: Option<ProcessChunkFn<Ctx>>,
    pool: Option<BytePool>,
}

impl Default for AssetStoreBuilder<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetStoreBuilder<()> {
    /// Builder with defaults (no root_dir/asset_root/evict/cancel/process set).
    pub fn new() -> Self {
        // Default pass-through process_fn for () - just copies input to output
        let dummy_process: ProcessChunkFn<()> = Arc::new(|input, output, _ctx, _is_last| {
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        });

        Self {
            root_dir: None,
            asset_root: None,
            evict_config: None,
            cancel: None,
            process_fn: Some(dummy_process),
            pool: None,
        }
    }
}

impl<Ctx> AssetStoreBuilder<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + std::fmt::Debug + 'static,
{
    /// Set the root directory for the asset store.
    pub fn root_dir(mut self, root: impl Into<PathBuf>) -> Self {
        self.root_dir = Some(root.into());
        self
    }

    /// Set the asset root identifier (e.g. from `asset_root_for_url`).
    pub fn asset_root(mut self, asset_root: impl Into<String>) -> Self {
        self.asset_root = Some(asset_root.into());
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

    /// Set the buffer pool (created at application startup and shared).
    pub fn pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Build the asset store.
    ///
    /// # Panics
    /// Panics if `asset_root` is not set or if `process_fn` is not set for Ctx != ().
    pub fn build(self) -> AssetStore<Ctx> {
        let root_dir = self.root_dir.unwrap_or_else(|| {
            tempdir()
                .expect("failed to create AssetStore temp dir")
                .keep()
        });
        let asset_root = self
            .asset_root
            .expect("asset_root is required for AssetStoreBuilder");
        let evict_cfg = self.evict_config.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();

        let process_fn = self
            .process_fn
            .expect("process_fn is required for AssetStoreBuilder");

        // Use provided pool or global pool
        let pool = self.pool.unwrap_or_else(|| byte_pool().clone());

        // Build decorator chain: Disk -> Evict -> Processing -> Lease -> Cached
        let disk = Arc::new(DiskAssetStore::new(root_dir, asset_root, cancel.clone()));
        let evict = Arc::new(EvictAssets::new(disk, evict_cfg, cancel.clone()));
        let processing = Arc::new(ProcessingAssets::new(evict.clone(), process_fn, pool));

        // LeaseAssets holds evict for byte recording
        let lease = LeaseAssets::with_byte_recorder(
            processing,
            cancel,
            evict as Arc<dyn crate::evict::ByteRecorder>,
        );

        // CachedAssets on top to cache LeaseResource with guards
        CachedAssets::new(Arc::new(lease))
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
            root_dir: self.root_dir,
            asset_root: self.asset_root,
            evict_config: self.evict_config,
            cancel: self.cancel,
            process_fn: Some(f),
            pool: self.pool,
        }
    }
}
