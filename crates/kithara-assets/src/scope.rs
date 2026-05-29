#![forbid(unsafe_code)]

//! Key factory bound to one `asset_root` over a shared [`AssetStore`].

use std::{fmt::Debug, hash::Hash, sync::Arc};

use url::Url;

use crate::{error::AssetsResult, key::ResourceKey, unified::AssetStore};

/// A lightweight handle that holds one `asset_root` over a shared
/// [`AssetStore`] and mints self-identifying [`ResourceKey`]s under it.
/// Cloning is cheap - the backing store is shared, not copied.
///
/// Obtain one via [`AssetStore::scope`]. Per-resource operations live on
/// the store ([`AssetStore::open_resource`] and friends) and take a
/// self-contained `&ResourceKey`. Asset-level operations
/// ([`AssetScope::delete_asset`]) stay here.
#[derive(Clone, Debug)]
pub struct AssetScope<Ctx = ()>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    store: AssetStore<Ctx>,
    asset_root: Arc<str>,
}

impl<Ctx> AssetScope<Ctx>
where
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    pub(crate) fn new(store: AssetStore<Ctx>, asset_root: Arc<str>) -> Self {
        Self { store, asset_root }
    }

    /// The `asset_root` this scope is bound to.
    #[must_use]
    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }

    /// The underlying shared store, where per-resource operations live.
    #[must_use]
    pub fn store(&self) -> &AssetStore<Ctx> {
        &self.store
    }

    /// Mint a relative key for `rel_path` under this scope's `asset_root`.
    #[must_use]
    pub fn key<P: Into<Arc<str>>>(&self, rel_path: P) -> ResourceKey {
        ResourceKey::relative(Arc::clone(&self.asset_root), rel_path)
    }

    /// Mint a relative key from a URL under this scope's `asset_root`.
    #[must_use]
    pub fn key_from_url(&self, url: &Url) -> ResourceKey {
        ResourceKey::relative(
            Arc::clone(&self.asset_root),
            ResourceKey::rel_path_from_url(url),
        )
    }

    /// Delete this entire asset (all resources under its `asset_root`).
    ///
    /// # Errors
    /// Returns `AssetsError` if the asset directory cannot be removed.
    pub fn delete_asset(&self) -> AssetsResult<()> {
        self.store.delete_asset(&self.asset_root)
    }
}
