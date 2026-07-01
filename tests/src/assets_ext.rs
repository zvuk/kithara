use kithara_assets::{AssetResourceState, AssetStore, ResourceKey};

/// Test-only convenience over the public [`AssetStore::resource_state`]:
/// "is there a committed resource for this key?". Production code inspects
/// the full [`AssetResourceState`] directly, so this committed-only shortcut
/// lives in the test harness.
pub trait AssetStoreTestExt {
    /// `true` when the key resolves to a committed resource.
    fn has_resource(&self, key: &ResourceKey) -> bool;
}

impl AssetStoreTestExt for AssetStore {
    fn has_resource(&self, key: &ResourceKey) -> bool {
        matches!(
            self.resource_state(key),
            Ok(AssetResourceState::Committed { .. })
        )
    }
}
