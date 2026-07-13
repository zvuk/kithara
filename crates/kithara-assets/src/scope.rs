#![forbid(unsafe_code)]

use kithara_platform::sync::Arc;
use url::Url;

use crate::{error::AssetsResult, key::ResourceKey, layout::AssetLayout, unified::AssetStore};

/// A lightweight handle that holds one `asset_root` over a shared
/// [`AssetStore`] and mints self-identifying [`ResourceKey`]s under it.
/// Cloning is cheap - the backing store is shared, not copied.
///
/// Obtain one via [`AssetStore::scope`]. Per-resource operations live on
/// the store ([`AssetStore::open_resource`] and friends) and take a
/// self-contained `&ResourceKey`. Asset-level operations
/// ([`AssetScope::delete_asset`]) stay here.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AssetScope {
    /// The `asset_root` this scope is bound to.
    #[field(get)]
    asset_root: Arc<str>,
    layout: Arc<dyn AssetLayout>,
    /// The underlying shared store, where per-resource operations live.
    #[field(get)]
    store: AssetStore,
}

impl AssetScope {
    pub(crate) fn new(store: AssetStore, asset_root: Arc<str>) -> Self {
        let layout = Arc::clone(store.layout());
        Self::with_layout(store, asset_root, layout)
    }

    /// Delete this entire asset (all resources under its `asset_root`).
    ///
    /// # Errors
    /// Returns `AssetsError` if the asset directory cannot be removed.
    pub fn delete_asset(&self) -> AssetsResult<()> {
        self.store.delete_asset(&self.asset_root)
    }

    /// Mint a relative key for `rel_path` under this scope's `asset_root`.
    #[must_use]
    pub fn key<P: Into<Arc<str>>>(&self, rel_path: P) -> ResourceKey {
        ResourceKey::relative(Arc::clone(&self.asset_root), rel_path)
    }

    /// Mint a relative key for the resource at `url` under this scope's `asset_root`.
    #[must_use]
    pub fn key_for(&self, url: &Url) -> ResourceKey {
        ResourceKey::relative(Arc::clone(&self.asset_root), self.layout.rel_path(url))
    }

    /// The underlying shared store, where per-resource operations live.
    pub(crate) fn with_layout(
        store: AssetStore,
        asset_root: Arc<str>,
        layout: Arc<dyn AssetLayout>,
    ) -> Self {
        Self {
            asset_root,
            layout,
            store,
        }
    }
}
