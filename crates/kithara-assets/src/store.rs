#![forbid(unsafe_code)]

use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use dashmap::DashMap;
use kithara_storage::{AtomicOptions, AtomicResource, DiskOptions, StreamingResource};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crate::{
    cache::Assets,
    error::{AssetsError, AssetsResult},
    evict::EvictAssets,
    index::EvictConfig,
    key::ResourceKey,
    lease::LeaseAssets,
};

/// Concrete on-disk [`Assets`] implementation for a single asset.
///
/// ## Normative
/// - This type is responsible for mapping [`ResourceKey`] → disk paths under a root directory.
/// - Each `DiskAssetStore` is scoped to a single `asset_root` (e.g. hash of master playlist URL).
/// - `kithara-assets` crate does not "invent" keys; it only *maps* them.
/// - Path mapping must be safe (no absolute paths, no `..`, no empty segments).
/// - This is not a "cache" by name or responsibility; caching/eviction are higher-level policies.
///
/// Note: this type is intentionally small and dumb: it does not implement pinning or eviction.
/// Pinning is provided by the `LeaseAssets` decorator.
/// Eviction is provided by the `EvictAssets` decorator.
#[derive(Clone, Debug)]
pub struct DiskAssetStore {
    root_dir: PathBuf,
    asset_root: String,
    cancel: CancellationToken,
    map: DashMap<CacheKey, Arc<ResourceEntry>>,
}

#[derive(Clone, Debug)]
pub struct AssetStore(LeaseAssets<EvictAssets<DiskAssetStore>>);

impl AssetStore {
    pub fn new(base: Arc<EvictAssets<DiskAssetStore>>, cancel: CancellationToken) -> Self {
        Self(LeaseAssets::new(base, cancel))
    }

    pub fn base(&self) -> &EvictAssets<DiskAssetStore> {
        self.0.base()
    }

    pub fn asset_root(&self) -> &str {
        self.0.base().base().asset_root()
    }
}

impl Deref for AssetStore {
    type Target = LeaseAssets<EvictAssets<DiskAssetStore>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Constructor for the ready-to-use [`AssetStore`].
///
/// ## Usage
/// ```ignore
/// let store = AssetStoreBuilder::new()
///     .root_dir("/path/to/cache")
///     .asset_root(asset_root_for_url(&master_url))
///     .build();
/// ```
///
/// ## Decorator order (normative)
/// - `EvictAssets` is applied before `LeaseAssets` so eviction is evaluated at "asset creation time"
///   without being affected by the new handle's pin.
/// - `LeaseAssets` provides RAII pinning for opened resources.
pub struct AssetStoreBuilder {
    root_dir: Option<PathBuf>,
    asset_root: Option<String>,
    evict_config: Option<EvictConfig>,
    cancel: Option<CancellationToken>,
}

impl Default for AssetStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetStoreBuilder {
    /// Builder with defaults (no root_dir/asset_root/evict/cancel set).
    pub fn new() -> Self {
        Self {
            root_dir: None,
            asset_root: None,
            evict_config: None,
            cancel: None,
        }
    }

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
    pub fn build(self) -> AssetStore {
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
        AssetStore::new(evict, cancel)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum CacheKey {
    Streaming(ResourceKey),
    Atomic(ResourceKey),
    PinsIndex,
    LruIndex,
}

#[derive(Clone, Debug)]
enum ResourceEntry {
    Streaming(StreamingResource),
    Atomic(AtomicResource),
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir` for a specific `asset_root`.
    pub fn new(
        root_dir: impl Into<PathBuf>,
        asset_root: impl Into<String>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            root_dir: root_dir.into(),
            asset_root: asset_root.into(),
            cancel,
            map: DashMap::new(),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }

    fn resource_path(&self, key: &ResourceKey) -> AssetsResult<PathBuf> {
        let asset_root = sanitize_rel(&self.asset_root).map_err(|()| AssetsError::InvalidKey)?;
        let rel_path = sanitize_rel(key.rel_path()).map_err(|()| AssetsError::InvalidKey)?;
        Ok(self.root_dir.join(asset_root).join(rel_path))
    }

    fn pins_index_path(&self) -> PathBuf {
        // The pins index location is an internal detail of this concrete store.
        // Higher layers must not hardcode keys/paths for it.
        self.root_dir.join("_index").join("pins.json")
    }

    fn lru_index_path(&self) -> PathBuf {
        // The LRU index location is an internal detail of this concrete store.
        // Higher layers must not hardcode keys/paths for it.
        self.root_dir.join("_index").join("lru.json")
    }

    fn cached_atomic(&self, cache_key: &CacheKey) -> Option<AtomicResource> {
        self.map
            .get(cache_key)
            .and_then(|entry| match entry.value().as_ref() {
                ResourceEntry::Atomic(res) => Some(res.clone()),
                ResourceEntry::Streaming(_) => None,
            })
    }

    fn store_atomic(&self, cache_key: CacheKey, res: AtomicResource) -> AtomicResource {
        match self.map.entry(cache_key) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => match entry.get().as_ref() {
                ResourceEntry::Atomic(existing) => existing.clone(),
                ResourceEntry::Streaming(_) => {
                    entry.insert(Arc::new(ResourceEntry::Atomic(res.clone())));
                    res
                }
            },
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(Arc::new(ResourceEntry::Atomic(res.clone())));
                res
            }
        }
    }

    fn cached_streaming(&self, cache_key: &CacheKey) -> Option<StreamingResource> {
        self.map
            .get(cache_key)
            .and_then(|entry| match entry.value().as_ref() {
                ResourceEntry::Streaming(res) => Some(res.clone()),
                ResourceEntry::Atomic(_) => None,
            })
    }

    fn store_streaming(&self, cache_key: CacheKey, res: StreamingResource) -> StreamingResource {
        match self.map.entry(cache_key) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => match entry.get().as_ref() {
                ResourceEntry::Streaming(existing) => existing.clone(),
                ResourceEntry::Atomic(_) => {
                    entry.insert(Arc::new(ResourceEntry::Streaming(res.clone())));
                    res
                }
            },
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(Arc::new(ResourceEntry::Streaming(res.clone())));
                res
            }
        }
    }
}

#[async_trait]
impl Assets for DiskAssetStore {
    fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn asset_root(&self) -> &str {
        &self.asset_root
    }

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<AtomicResource> {
        let cache_key = CacheKey::Atomic(key.clone());

        if let Some(existing) = self.cached_atomic(&cache_key) {
            return Ok(existing);
        }

        let path = self.resource_path(key)?;
        let res = AtomicResource::open(AtomicOptions {
            path,
            cancel: self.cancel.clone(),
        });

        Ok(self.store_atomic(cache_key, res))
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<StreamingResource> {
        let cache_key = CacheKey::Streaming(key.clone());

        if let Some(existing) = self.cached_streaming(&cache_key) {
            return Ok(existing);
        }

        let path = self.resource_path(key)?;
        let res = StreamingResource::open_disk(DiskOptions {
            path,
            cancel: self.cancel.clone(),
            initial_len: None,
        })
        .await?;

        Ok(self.store_streaming(cache_key, res))
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        if let Some(existing) = self.cached_atomic(&CacheKey::PinsIndex) {
            return Ok(existing);
        }

        let path = self.pins_index_path();
        let res = AtomicResource::open(AtomicOptions {
            path,
            cancel: self.cancel.clone(),
        });

        Ok(self.store_atomic(CacheKey::PinsIndex, res))
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(kithara_storage::StorageError::Cancelled.into());
        }

        // Clean cache entries for this asset.
        self.map
            .retain(|k, _| matches!(k, CacheKey::PinsIndex | CacheKey::LruIndex));

        // Delete the asset directory.
        delete_asset_dir(&self.root_dir, &self.asset_root)
            .await
            .map_err(|e| e.into())
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        if let Some(existing) = self.cached_atomic(&CacheKey::LruIndex) {
            return Ok(existing);
        }

        let path = self.lru_index_path();
        let res = AtomicResource::open(AtomicOptions {
            path,
            cancel: self.cancel.clone(),
        });

        Ok(self.store_atomic(CacheKey::LruIndex, res))
    }
}

/// Delete an asset directory by `asset_root` directly via filesystem.
///
/// This is a low-level operation used by eviction to delete other assets.
/// It does NOT clear any in-memory caches (the caller is responsible for that if needed).
pub(crate) async fn delete_asset_dir(root_dir: &Path, asset_root: &str) -> std::io::Result<()> {
    let safe = sanitize_rel(asset_root).map_err(|()| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid asset_root")
    })?;
    let path = root_dir.join(safe);
    match tokio::fs::remove_dir_all(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub(crate) fn sanitize_rel(input: &str) -> Result<String, ()> {
    // Minimal normalization: treat backslashes as separators to avoid Windows traversal surprises.
    let s = input.replace('\\', "/");
    if s.is_empty() || s.starts_with('/') || s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }
    Ok(s)
}
