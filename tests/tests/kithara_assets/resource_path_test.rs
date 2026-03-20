#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, AssetStoreBuilder, EvictConfig, ResourceKey},
    storage::ResourceExt,
};
use kithara_platform::time::Duration;
use kithara_test_utils::{TestTempDir, temp_dir};

// Test Fixtures

fn asset_store_with_root(temp_dir: &TestTempDir, asset_root: &str) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(Some(asset_root))
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build()
}

// AssetResource Path Tests

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn asset_resource_path_method(temp_dir: TestTempDir) {
    let asset_store = asset_store_with_root(&temp_dir, "test-asset");
    let key = ResourceKey::new("metadata.json");
    let asset_resource = asset_store
        .acquire_resource(&key)
        .expect("Failed to open resource");

    // Write some data to ensure the resource is properly initialized
    asset_resource
        .write_all(b"test data")
        .expect("Write should succeed");

    // Get the path from AssetResource
    let asset_path = asset_resource.path().unwrap();

    // Get the root directory from the store
    let root_dir = asset_store.root_dir();

    // Check that AssetResource's path is under the store's root directory
    assert!(asset_path.starts_with(root_dir));
    assert!(asset_path.ends_with("test-asset/metadata.json"));

    // Verify the path components
    assert!(asset_path.parent().unwrap().ends_with("test-asset"));
    assert!(asset_path.file_name().unwrap() == "metadata.json");
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn asset_resource_streaming_path_method(temp_dir: TestTempDir) {
    let asset_store = asset_store_with_root(&temp_dir, "test-asset");
    let key = ResourceKey::new("media.bin");
    let asset_resource = asset_store
        .acquire_resource(&key)
        .expect("Failed to open resource");

    // Write some data to ensure the resource is properly initialized
    asset_resource
        .write_at(0, b"test data")
        .expect("Write should succeed");
    asset_resource
        .commit(Some(b"test data".len() as u64))
        .expect("Commit should succeed");

    // Get the path from AssetResource
    let asset_path = asset_resource.path().unwrap();

    // Get the root directory from the store
    let root_dir = asset_store.root_dir();

    // Check that AssetResource's path is under the store's root directory
    assert!(asset_path.starts_with(root_dir));
    assert!(asset_path.ends_with("test-asset/media.bin"));

    // Verify the path components
    assert!(asset_path.parent().unwrap().ends_with("test-asset"));
    assert!(asset_path.file_name().unwrap() == "media.bin");
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn asset_resource_path_consistency(temp_dir: TestTempDir) {
    let asset_store = asset_store_with_root(&temp_dir, "test-asset");
    let key = ResourceKey::new("data.bin");
    let asset_resource = asset_store
        .acquire_resource(&key)
        .expect("Failed to open resource");

    let asset_path = asset_resource.path().unwrap();

    // Just verify the path is accessible
    assert!(!asset_path.as_os_str().is_empty());

    // Write data
    asset_resource
        .write_all(b"test data")
        .expect("Write should succeed");

    // Path should still be accessible after write
    assert!(!asset_resource.path().unwrap().as_os_str().is_empty());
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn asset_resource_path_reflects_asset_root_and_resource_name(temp_dir: TestTempDir) {
    let asset_root = "my-asset";
    let resource_name = "subdir/file.txt";
    let asset_store = asset_store_with_root(&temp_dir, asset_root);
    let key = ResourceKey::new(resource_name);
    let asset_resource = asset_store
        .acquire_resource(&key)
        .expect("Failed to open resource");

    let path = asset_resource.path().unwrap();
    let root_dir = asset_store.root_dir();

    // Check that the path is under the store's root directory
    assert!(path.starts_with(root_dir));

    // Verify the path components
    assert!(path.ends_with(resource_name));
    assert!(path.to_string_lossy().contains(asset_root));

    // Check that the path includes both asset_root and resource_name
    let relative_path = path.strip_prefix(root_dir).unwrap();
    assert!(relative_path.starts_with(asset_root));
    assert!(relative_path.ends_with(resource_name));
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn multiple_resources_same_asset_root_have_different_paths(temp_dir: TestTempDir) {
    let asset_root = "shared-asset";
    let asset_store = asset_store_with_root(&temp_dir, asset_root);

    // Create two different resources in the same asset root
    let key1 = ResourceKey::new("resource1.bin");
    let resource1 = asset_store
        .acquire_resource(&key1)
        .expect("Failed to open resource1");

    let key2 = ResourceKey::new("resource2.bin");
    let resource2 = asset_store
        .acquire_resource(&key2)
        .expect("Failed to open resource2");

    // Paths should be different
    assert_ne!(resource1.path(), resource2.path());

    // But they should share the same parent directory (asset root)
    assert_eq!(
        resource1.path().unwrap().parent(),
        resource2.path().unwrap().parent()
    );

    // Both should be under the asset root directory
    assert!(
        resource1
            .path()
            .unwrap()
            .parent()
            .unwrap()
            .ends_with(asset_root)
    );
    assert!(
        resource2
            .path()
            .unwrap()
            .parent()
            .unwrap()
            .ends_with(asset_root)
    );
}
