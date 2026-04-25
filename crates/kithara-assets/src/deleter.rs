#![forbid(unsafe_code)]

//! Single-channel removal API for `kithara-assets`.
//!
//! Every path that physically deletes a resource (one file, an entire
//! `asset_root` directory, or the in-memory `active_resources` map for
//! the mem backend) goes through [`AssetDeleter`]. The contract is:
//!
//! 1. Physical deletion (`FS`, `DashMap` entry) and
//!    [`AvailabilityIndex`](crate::index::AvailabilityIndex) invalidation
//!    happen atomically inside one method call.
//! 2. Direct [`std::fs::remove_file`] / [`std::fs::remove_dir_all`] and
//!    direct manipulation of the mem backend's `active_resources` map
//!    are forbidden anywhere outside the implementations of this trait.
//!
//! This closes the bypass family pinned by
//! `red_test_lease_resource_drop_strands_availability_index` and
//! `red_test_delete_asset_strands_availability_index` in
//! [`crate::store::tests`].

use crate::{error::AssetsResult, key::ResourceKey};

/// Trait implemented by the disk and mem backends to expose a single
/// canonical removal channel. See module docs for the contract.
pub(crate) trait AssetDeleter: Send + Sync + std::fmt::Debug {
    /// Remove a single resource identified by (`asset_root`, `key`).
    ///
    /// Disk impl: `fs::remove_file` of the resolved path, then
    /// `availability.remove(asset_root, key)`.
    ///
    /// Mem impl: drop the matching `active_resources` entry (only when
    /// `asset_root` matches the backend's own root — mem backends are
    /// scoped to a single `asset_root`), then `availability.remove`.
    fn remove_resource(&self, asset_root: &str, key: &ResourceKey) -> AssetsResult<()>;

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
}
