#![forbid(unsafe_code)]

use std::{ops::Deref, path::PathBuf, sync::Arc};

use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crate::{
    base::DiskAssetStore,
    cache::CachedAssets,
    evict::EvictAssets,
    index::EvictConfig,
    lease::LeaseAssets,
};

#[derive(Clone, Debug)]
pub struct AssetStore(LeaseAssets<CachedAssets<EvictAssets<DiskAssetStore>>>);

impl AssetStore {
    pub fn new(
        base: Arc<CachedAssets<EvictAssets<DiskAssetStore>>>,
        cancel: CancellationToken,
    ) -> Self {
        Self(LeaseAssets::new(base, cancel))
    }

    pub fn base(&self) -> &CachedAssets<EvictAssets<DiskAssetStore>> {
        self.0.base()
    }

    pub fn asset_root(&self) -> &str {
        self.0.base().base().base().asset_root()
    }
}

impl Deref for AssetStore {
    type Target = LeaseAssets<CachedAssets<EvictAssets<DiskAssetStore>>>;

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
/// - `EvictAssets` is applied first (evaluates eviction at "asset creation time")
/// - `CachedAssets` caches opened resources in memory
/// - `LeaseAssets` provides RAII pinning for opened resources (outermost)
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
        let cached = Arc::new(CachedAssets::new(evict));
        AssetStore::new(cached, cancel)
    }
}
