#![forbid(unsafe_code)]

use kithara_platform::sync::Arc;

use super::{
    AssetLayout, AssetResource, AssetSource, ResourceKey, validate_path, validate_root,
    validate_source,
};
use crate::{error::AssetsResult, store::AssetStore};

/// A store handle bound to one layout-selected asset root.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AssetScope {
    #[field(get)]
    asset_root: Arc<str>,
    layout: Arc<dyn AssetLayout>,
    #[field(get)]
    store: AssetStore,
}

impl AssetScope {
    pub(crate) fn new(
        store: AssetStore,
        source: &AssetSource,
        layout: Arc<dyn AssetLayout>,
    ) -> AssetsResult<Self> {
        validate_source(source)?;
        let asset_root = layout.root(source);
        validate_root(&asset_root)?;
        Ok(Self {
            asset_root: Arc::from(asset_root),
            layout,
            store,
        })
    }

    /// Delete this asset and every resource below its layout-owned root.
    ///
    /// # Errors
    /// Returns an error when the backing asset cannot be removed.
    pub fn delete_asset(&self) -> AssetsResult<()> {
        self.store.delete_asset(&self.asset_root)
    }

    /// Mint a validated key using the scope's selected layout.
    ///
    /// # Errors
    /// Returns [`crate::AssetsError::InvalidKey`] for hostile layout output.
    pub fn key(&self, resource: &AssetResource) -> AssetsResult<ResourceKey> {
        let path = self.layout.path(resource);
        validate_path(&path)?;
        Ok(ResourceKey::relative(Arc::clone(&self.asset_root), path))
    }
}
