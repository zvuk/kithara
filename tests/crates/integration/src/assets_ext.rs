#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use kithara::assets::{AssetResourceState, AssetStore, ResourceKey, StorageBackend};

use crate::bufpool_ext::{TestPools, pools};

#[cfg(not(target_arch = "wasm32"))]
pub fn disk_asset_store(root: impl Into<PathBuf>) -> AssetStore<TestPools> {
    AssetStore::builder(pools())
        .backend(StorageBackend::Disk { root: root.into() })
        .build()
}

pub fn memory_asset_store() -> AssetStore<TestPools> {
    AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .build()
}

/// Test-only convenience over the public [`AssetStore::resource_state`]:
/// "is there a committed resource for this key?". Production code inspects
/// the full [`AssetResourceState`] directly, so this committed-only shortcut
/// lives in the test harness.
pub trait AssetStoreTestExt {
    /// `true` when the key resolves to a committed resource.
    fn has_resource(&self, key: &ResourceKey) -> bool;
}

impl AssetStoreTestExt for AssetStore<TestPools> {
    fn has_resource(&self, key: &ResourceKey) -> bool {
        matches!(
            self.resource_state(key),
            Ok(AssetResourceState::Committed { .. })
        )
    }
}
