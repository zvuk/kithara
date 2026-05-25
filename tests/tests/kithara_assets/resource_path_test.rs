#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetScope, AssetStoreBuilder, EvictConfig},
    storage::ResourceExt,
};
use kithara_integration_tests::{TestTempDir, temp_dir};
use kithara_platform::time::Duration;

fn asset_scope_with_root(temp_dir: &TestTempDir, asset_root: &str) -> AssetScope {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build()
        .scope(asset_root)
}

/// Writes to `asset_resource` via either `write_all` (atomic) or
/// `write_at` + `commit` (streaming) depending on the case.
enum WriteMode {
    Atomic,
    Streaming,
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::atomic("metadata.json", WriteMode::Atomic)]
#[case::streaming("media.bin", WriteMode::Streaming)]
fn asset_resource_path_method(
    temp_dir: TestTempDir,
    #[case] resource_name: &str,
    #[case] write_mode: WriteMode,
) {
    let scope = asset_scope_with_root(&temp_dir, "test-asset");
    let key = scope.key(resource_name);
    let asset_resource = scope
        .store()
        .acquire_resource(&key, None)
        .expect("Failed to open resource");

    match write_mode {
        WriteMode::Atomic => {
            asset_resource
                .write_all(b"test data")
                .expect("Write should succeed");
        }
        WriteMode::Streaming => {
            asset_resource
                .write_at(0, b"test data")
                .expect("Write should succeed");
            asset_resource
                .commit(Some(b"test data".len() as u64))
                .expect("Commit should succeed");
        }
    }

    let asset_path = asset_resource.path().unwrap();
    let root_dir = scope.store().root_dir();

    assert!(asset_path.starts_with(root_dir));
    let expected_suffix = format!("test-asset/{resource_name}");
    assert!(asset_path.ends_with(&expected_suffix));

    assert!(asset_path.parent().unwrap().ends_with("test-asset"));
    assert!(asset_path.file_name().unwrap() == resource_name);
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn asset_resource_path_consistency(temp_dir: TestTempDir) {
    let scope = asset_scope_with_root(&temp_dir, "test-asset");
    let key = scope.key("data.bin");
    let asset_resource = scope
        .store()
        .acquire_resource(&key, None)
        .expect("Failed to open resource");

    let asset_path = asset_resource.path().unwrap();

    assert!(!asset_path.as_os_str().is_empty());

    asset_resource
        .write_all(b"test data")
        .expect("Write should succeed");

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
    let scope = asset_scope_with_root(&temp_dir, asset_root);
    let key = scope.key(resource_name);
    let asset_resource = scope
        .store()
        .acquire_resource(&key, None)
        .expect("Failed to open resource");

    let path = asset_resource.path().unwrap();
    let root_dir = scope.store().root_dir();

    assert!(path.starts_with(root_dir));

    assert!(path.ends_with(resource_name));
    assert!(path.to_string_lossy().contains(asset_root));

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
    let scope = asset_scope_with_root(&temp_dir, asset_root);

    let key1 = scope.key("resource1.bin");
    let resource1 = scope
        .store()
        .acquire_resource(&key1, None)
        .expect("Failed to open resource1");

    let key2 = scope.key("resource2.bin");
    let resource2 = scope
        .store()
        .acquire_resource(&key2, None)
        .expect("Failed to open resource2");

    assert_ne!(resource1.path(), resource2.path());

    assert_eq!(
        resource1.path().unwrap().parent(),
        resource2.path().unwrap().parent()
    );

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
