#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use kithara_storage::{AtomicOptions, AtomicResource, DiskOptions, StreamingResource};
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
}

/// Ready-to-use assets store: `DiskAssetStore` composed with eviction + pin/lease.
///
/// This is a type alias (no new wrapper type).
pub type AssetStore = LeaseAssets<EvictAssets<DiskAssetStore>>;

/// Constructor for the ready-to-use [`AssetStore`].
///
/// We use a free function (not `AssetStore::new`) because `AssetStore` is a type alias.
///
/// Decorator order (normative):
/// - `EvictAssets` is applied before `LeaseAssets` so eviction is evaluated at "asset creation time"
///   without being affected by the new handle's pin.
/// - `LeaseAssets` provides RAII pinning for opened resources.
impl AssetStore {
    pub fn with_root_dir(root_dir: impl Into<PathBuf>, cfg: EvictConfig) -> Self {
        let base = Arc::new(DiskAssetStore::new(root_dir));
        let evict = Arc::new(EvictAssets::new(base, cfg));
        Self::new(evict)
    }
}

impl DiskAssetStore {
    /// Create a store rooted at `root_dir`.
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
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
}

#[async_trait]
impl Assets for DiskAssetStore {
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource> {
        let path = self.resource_path(key)?;
        Ok(AtomicResource::open(AtomicOptions { path, cancel }))
    }

    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetsResult<StreamingResource> {
        let path = self.resource_path(key)?;
        let res = StreamingResource::open_disk(DiskOptions {
            path,
            cancel,
            initial_len: None,
        })
        .await?;
        Ok(res)
    }

    async fn open_pins_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource> {
        let path = self.pins_index_path();
        Ok(AtomicResource::open(AtomicOptions { path, cancel }))
    }

    async fn delete_asset(&self, asset_root: &str, cancel: CancellationToken) -> AssetsResult<()> {
        if cancel.is_cancelled() {
            return Err(kithara_storage::StorageError::Cancelled.into());
        }

        let path = self.asset_root_path(asset_root)?;

        // Best-effort: if the directory doesn't exist, treat as already deleted.
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn open_lru_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource> {
        let path = self.lru_index_path();
        Ok(AtomicResource::open(AtomicOptions { path, cancel }))
    }
}

fn sanitize_rel(input: &str) -> Result<String, ()> {
    // Minimal normalization: treat backslashes as separators to avoid Windows traversal surprises.
    let s = input.replace('\\', "/");

    if s.is_empty() || s.starts_with('/') {
        return Err(());
    }

    if s.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(());
    }

    Ok(s)
}
