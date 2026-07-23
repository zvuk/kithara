#![forbid(unsafe_code)]

use crate::{error::AssetsResult, layout::ResourceKey};

/// Trait implemented by the disk and mem backends to expose a single
/// canonical removal channel. See module docs for the contract.
pub(crate) trait AssetDeleter: Send + Sync + std::fmt::Debug {
    /// Remove an entire `asset_root` worth of state.
    ///
    /// Disk impl: `fs::remove_dir_all(root_dir/asset_root)`, then
    /// `availability.clear_root(asset_root)`. Used for own-asset
    /// teardown (`AssetStore::delete_asset`) and foreign-asset eviction
    /// (`EvictAssets::evict_one`, `EvictAssets::on_asset_created`).
    ///
    /// Mem impl: clear the `active_resources` map only when
    /// `asset_root` matches the backend's own root, then
    /// `availability.clear_root(asset_root)`. Foreign-asset deletion
    /// from the mem backend just invalidates the index — the
    /// per-resource handles for a foreign `asset_root` live in a
    /// different mem backend instance and are unreachable from here.
    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;

    /// Remove a single resource identified by `key`.
    ///
    /// Disk impl: `fs::remove_file` of the resolved path, then
    /// `availability.remove(key)`.
    ///
    /// Mem impl: drop the matching `active_resources` entry, then
    /// `availability.remove(key)`.
    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
}
