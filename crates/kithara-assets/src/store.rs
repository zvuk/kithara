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

/// Concrete on-disk [`Assets`] implementation.
///
/// ## Normative
/// - This type is responsible for mapping [`ResourceKey`] → disk paths under a root directory.
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
}

impl Deref for AssetStore {
    type Target = LeaseAssets<EvictAssets<DiskAssetStore>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Constructor for the ready-to-use [`AssetStore`].
///
/// We use a free function (not `AssetStore::new`) because `AssetStore` is a type alias.
///
/// Decorator order (normative):
/// - `EvictAssets` is applied before `LeaseAssets` so eviction is evaluated at "asset creation time"
///   without being affected by the new handle's pin.
/// - `LeaseAssets` provides RAII pinning for opened resources.
#[derive(Default)]
pub struct AssetStoreBuilder {
    root_dir: Option<PathBuf>,
    evict_config: Option<EvictConfig>,
    cancel: Option<CancellationToken>,
}

impl AssetStoreBuilder {
    /// Builder with defaults (no root_dir/evict/cancel set).
    pub fn new() -> Self {
        Self {
            root_dir: None,
            evict_config: None,
            cancel: None,
        }
    }

    pub fn evict_config(mut self, cfg: EvictConfig) -> Self {
        self.evict_config = Some(cfg);
        self
    }

    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn root_dir(mut self, root: impl Into<PathBuf>) -> Self {
        self.root_dir = Some(root.into());
        self
    }

    pub fn build(self) -> AssetStore {
        #[allow(deprecated)]
        let root_dir = self.root_dir.unwrap_or_else(|| {
            tempdir()
                .expect("failed to create AssetStore temp dir")
                .into_path()
        });
        let evict_cfg = self.evict_config.unwrap_or_default();
        let cancel = self.cancel.unwrap_or_default();

        let base = Arc::new(DiskAssetStore::new(root_dir, cancel.clone()));
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
    /// Create a store rooted at `root_dir`.
    pub fn new(root_dir: impl Into<PathBuf>, cancel: CancellationToken) -> Self {
        Self {
            root_dir: root_dir.into(),
            cancel,
            map: DashMap::new(),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn resource_path(&self, key: &ResourceKey) -> AssetsResult<PathBuf> {
        let asset_root = sanitize_rel(&key.asset_root).map_err(|()| AssetsError::InvalidKey)?;
        let rel_path = sanitize_rel(&key.rel_path).map_err(|()| AssetsError::InvalidKey)?;
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

    fn asset_root_path(&self, asset_root: &str) -> AssetsResult<PathBuf> {
        let safe = sanitize_rel(asset_root).map_err(|()| AssetsError::InvalidKey)?;
        Ok(self.root_dir.join(safe))
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

    async fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(kithara_storage::StorageError::Cancelled.into());
        }

        let path = self.asset_root_path(asset_root)?;

        // Clean cache entries for this asset_root.
        self.map.retain(|k, _| match k {
            CacheKey::Atomic(k) | CacheKey::Streaming(k) => k.asset_root() != asset_root,
            CacheKey::PinsIndex | CacheKey::LruIndex => true,
        });

        // Best-effort: if the directory doesn't exist, treat as already deleted.
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
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

fn sanitize_rel(input: &str) -> Result<String, ()> {
    // Minimal normalization: treat backslashes as separators to avoid Windows traversal surprises.
    let s = input.replace('\\', "/");
    if s.is_empty() || s.starts_with('/') || s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }
    Ok(s)
}
