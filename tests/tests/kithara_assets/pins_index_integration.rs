#![forbid(unsafe_code)]

use std::{collections::HashSet, time::Duration};

use kithara_assets::{AssetStore, AssetStoreBuilder, DiskAssetStore, EvictConfig, PinsIndex};
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

use crate::common::fixtures::temp_dir;

fn pins_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("_index").join("pins.bin")
}

#[fixture]
fn asset_store_no_limits(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(Some("test-asset"))
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build()
}

#[fixture]
fn disk_asset_store(temp_dir: tempfile::TempDir) -> DiskAssetStore {
    DiskAssetStore::new(temp_dir.path(), "test-asset", CancellationToken::new())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_missing_returns_default(
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let dir = temp_dir.path();
    let base = disk_asset_store;

    let path = pins_path(dir);
    assert!(!path.exists(), "pins.bin must not exist initially");

    let idx = PinsIndex::open(&base).unwrap();
    let pins = idx.load().unwrap();

    assert!(
        pins.is_empty(),
        "missing pins index must be treated as empty (best-effort default)"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_invalid_json_returns_default(
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let dir = temp_dir.path();
    let base = disk_asset_store;

    // Write a corrupted JSON file directly on disk to simulate index damage.
    let index_dir = dir.join("_index");
    std::fs::create_dir_all(&index_dir).unwrap();

    let path = pins_path(dir);
    std::fs::write(&path, b"{ this is not valid json").unwrap();
    assert!(path.exists(), "pins.bin must exist for this test");

    let idx = PinsIndex::open(&base).unwrap();
    let pins = idx.load().unwrap();

    assert!(
        pins.is_empty(),
        "invalid JSON pins index must be treated as empty (best-effort default)"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_roundtrip_store_then_load(
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let _dir = temp_dir.path();
    let base = disk_asset_store;

    let idx = PinsIndex::open(&base).unwrap();

    let mut pins = HashSet::new();
    pins.insert("asset-a".to_string());
    pins.insert("asset-b".to_string());

    idx.store(&pins).unwrap();

    // A second instance reading the same underlying resource should see the persisted set.
    let idx2 = PinsIndex::open(&base).unwrap();
    let loaded = idx2.load().unwrap();

    assert_eq!(loaded, pins, "pins index must roundtrip via store/load");
}

#[rstest]
#[case(vec!["asset-a"])]
#[case(vec!["asset-a", "asset-b", "asset-c"])]
#[case(vec!["asset-1", "asset-2", "asset-3", "asset-4", "asset-5"])]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_store_load_with_different_sets(
    #[case] asset_names: Vec<&str>,
    _temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let base = disk_asset_store;

    let idx = PinsIndex::open(&base).unwrap();

    let pins: HashSet<String> = asset_names.iter().map(|s| s.to_string()).collect();
    idx.store(&pins).unwrap();

    let loaded = idx.load().unwrap();
    assert_eq!(loaded, pins, "pins index must preserve all entries");
}

#[rstest]
#[case(2)]
#[case(3)]
#[case(5)]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_concurrent_updates_handled_correctly(
    #[case] asset_count: usize,
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let _dir = temp_dir.path();
    let base = disk_asset_store;

    // Create first index and store some pins
    let idx1 = PinsIndex::open(&base).unwrap();
    let pins1: HashSet<String> = (0..asset_count)
        .map(|i| format!("asset-{}", i + 1))
        .collect();
    idx1.store(&pins1).unwrap();

    // Create second index and load (should see first pins)
    let idx2 = PinsIndex::open(&base).unwrap();
    let loaded1 = idx2.load().unwrap();
    assert_eq!(loaded1, pins1);

    // Update with second index
    let pins2: HashSet<String> = (0..asset_count)
        .map(|i| format!("asset-updated-{}", i + 1))
        .collect();
    idx2.store(&pins2).unwrap();

    // A fresh open should see updated pins
    let idx3 = PinsIndex::open(&base).unwrap();
    let loaded2 = idx3.load().unwrap();
    assert_eq!(loaded2, pins2);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_empty_set_stores_and_loads_correctly(
    _temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let base = disk_asset_store;

    let idx = PinsIndex::open(&base).unwrap();

    // Store empty set
    let empty_pins = HashSet::new();
    idx.store(&empty_pins).unwrap();

    // Load should return empty set
    let loaded = idx.load().unwrap();
    assert!(
        loaded.is_empty(),
        "empty pins set should roundtrip correctly"
    );

    // File format is an implementation detail (binary via bincode)
    // No need to verify internal structure
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn pins_index_persists_across_store_instances(temp_dir: tempfile::TempDir) {
    let dir = temp_dir.path();
    let cancel = CancellationToken::new();

    // Create first store and write pins
    let base1 = DiskAssetStore::new(dir, "test-asset", cancel.clone());
    let idx1 = PinsIndex::open(&base1).unwrap();

    let mut pins = HashSet::new();
    pins.insert("persisted-asset".to_string());
    pins.insert("another-asset".to_string());

    idx1.store(&pins).unwrap();

    // Create completely new store instance (simulating restart)
    let base2 = DiskAssetStore::new(dir, "test-asset", cancel);
    let idx2 = PinsIndex::open(&base2).unwrap();

    let loaded = idx2.load().unwrap();
    assert_eq!(loaded, pins, "pins should persist across store instances");
}
