#![forbid(unsafe_code)]

use std::{
    hash::Hash,
    marker::PhantomData,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use kithara_storage::AtomicResource;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crate::{
    AssetsResult,
    base::{Assets, DiskAssetStore},
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    key::ResourceKey,
    lease::LeaseAssets,
    processing::{ProcessFn, ProcessedResource, ProcessingAssets},
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

type InnerStore = LeaseAssets<CachedAssets<EvictAssets<DiskAssetStore>>>;

/// Ready-to-use asset store with optional processing layer.
///
/// Generic parameter `Ctx` is the context type for the processing callback.
/// Use `()` (default) for no processing.
#[derive(Clone)]
pub struct AssetStore<Ctx: Clone + Hash + Eq + Send + Sync + 'static = ()> {
    inner: InnerStore,
    processing: Option<Arc<ProcessingAssets<InnerStore, Ctx>>>,
    _marker: PhantomData<Ctx>,
}

impl<Ctx: Clone + Hash + Eq + Send + Sync + 'static> std::fmt::Debug for AssetStore<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetStore")
            .field("inner", &self.inner)
            .field("has_processing", &self.processing.is_some())
            .finish()
    }
}

impl<Ctx> AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + 'static,
{
    fn new(
        base: Arc<CachedAssets<EvictAssets<DiskAssetStore>>>,
        cancel: CancellationToken,
        process_fn: Option<ProcessFn<Ctx>>,
    ) -> Self {
        let inner = LeaseAssets::new(base, cancel);
        let processing = process_fn.map(|f| {
            Arc::new(ProcessingAssets::new(Arc::new(inner.clone()), f))
        });
        Self {
            inner,
            processing,
            _marker: PhantomData,
        }
    }

    pub fn base(&self) -> &CachedAssets<EvictAssets<DiskAssetStore>> {
        self.inner.base()
    }

    pub fn asset_root(&self) -> &str {
        self.inner.base().base().base().asset_root()
    }

    /// Open a processed streaming resource.
    ///
    /// Returns error if no processing callback was configured.
    pub async fn open_processed(
        &self,
        key: &ResourceKey,
        ctx: Ctx,
    ) -> AssetsResult<Arc<ProcessedResource<<InnerStore as Assets>::StreamingRes, Ctx>>> {
        let processing = self.processing.as_ref().ok_or(crate::AssetsError::InvalidKey)?;
        processing.open_processed(key, ctx).await
    }

    /// Check if processing is enabled.
    pub fn has_processing(&self) -> bool {
        self.processing.is_some()
    }
}

impl<Ctx: Clone + Hash + Eq + Send + Sync + 'static> Deref for AssetStore<Ctx> {
    type Target = InnerStore;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[async_trait]
impl<Ctx> Assets for AssetStore<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + 'static,
{
    type StreamingRes = <InnerStore as Assets>::StreamingRes;
    type AtomicRes = <InnerStore as Assets>::AtomicRes;

    fn root_dir(&self) -> &Path {
        self.inner.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.inner.asset_root()
    }

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<Self::AtomicRes> {
        self.inner.open_atomic_resource(key).await
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<Self::StreamingRes> {
        self.inner.open_streaming_resource(key).await
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.inner.open_pins_index_resource().await
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.inner.open_lru_index_resource().await
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset().await
    }
}

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
    process_fn: Option<ProcessFn<Ctx>>,
}

impl Default for AssetStoreBuilder<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetStoreBuilder<()> {
    /// Builder with defaults (no root_dir/asset_root/evict/cancel/process set).
    pub fn new() -> Self {
        Self {
            root_dir: None,
            asset_root: None,
            evict_config: None,
            cancel: None,
            process_fn: None,
        }
    }
}

impl<Ctx> AssetStoreBuilder<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + 'static,
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

    /// Build the asset store.
    ///
    /// # Panics
    /// Panics if `asset_root` is not set.
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

        let base = Arc::new(DiskAssetStore::new(root_dir, asset_root, cancel.clone()));
        let evict = Arc::new(EvictAssets::new(base, evict_cfg, cancel.clone()));
        let cached = Arc::new(CachedAssets::new(evict));
        AssetStore::new(cached, cancel, self.process_fn)
    }
}

impl<OldCtx: Clone + Hash + Eq + Send + Sync + 'static> AssetStoreBuilder<OldCtx> {
    /// Set the processing callback for transforming resources.
    ///
    /// This changes the builder's context type.
    pub fn process_fn<NewCtx>(self, f: ProcessFn<NewCtx>) -> AssetStoreBuilder<NewCtx>
    where
        NewCtx: Clone + Hash + Eq + Send + Sync + 'static,
    {
        AssetStoreBuilder {
            root_dir: self.root_dir,
            asset_root: self.asset_root,
            evict_config: self.evict_config,
            cancel: self.cancel,
            process_fn: Some(f),
        }
    }
}
