#![forbid(unsafe_code)]

use std::{collections::HashSet, time::Duration};

use kithara_assets::{AssetStore, DiskAssetStore, EvictConfig, PinsIndex};
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

fn pins_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("_index").join("pins.json")
}

#[fixture]
fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

#[fixture]
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[fixture]
fn asset_store_no_limits(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    )
}

#[fixture]
fn disk_asset_store(temp_dir: tempfile::TempDir) -> DiskAssetStore {
    DiskAssetStore::new(temp_dir.path())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_missing_returns_default(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let base = disk_asset_store;

    let path = pins_path(dir);
    assert!(!path.exists(), "pins.json must not exist initially");

    let idx = PinsIndex::open(&base, cancel).await.unwrap();
    let pins = idx.load().await.unwrap();

    assert!(
        pins.is_empty(),
        "missing pins index must be treated as empty (best-effort default)"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_invalid_json_returns_default(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let base = disk_asset_store;

    // Write a corrupted JSON file directly on disk to simulate index damage.
    let index_dir = dir.join("_index");
    std::fs::create_dir_all(&index_dir).unwrap();

    let path = pins_path(dir);
    std::fs::write(&path, b"{ this is not valid json").unwrap();
    assert!(path.exists(), "pins.json must exist for this test");

    let idx = PinsIndex::open(&base, cancel).await.unwrap();
    let pins = idx.load().await.unwrap();

    assert!(
        pins.is_empty(),
        "invalid JSON pins index must be treated as empty (best-effort default)"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_roundtrip_store_then_load(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let _dir = temp_dir.path();
    let cancel = cancel_token;
    let base = disk_asset_store;

    let idx = PinsIndex::open(&base, cancel).await.unwrap();

    let mut pins = HashSet::new();
    pins.insert("asset-a".to_string());
    pins.insert("asset-b".to_string());

    idx.store(&pins).await.unwrap();

    // A second instance reading the same underlying resource should see the persisted set.
    let idx2 = PinsIndex::open(&base, CancellationToken::new())
        .await
        .unwrap();
    let loaded = idx2.load().await.unwrap();

    assert_eq!(loaded, pins, "pins index must roundtrip via store/load");
}

#[rstest]
#[case(vec!["asset-a"])]
#[case(vec!["asset-a", "asset-b", "asset-c"])]
#[case(vec!["asset-1", "asset-2", "asset-3", "asset-4", "asset-5"])]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_store_load_with_different_sets(
    #[case] asset_names: Vec<&str>,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let cancel = cancel_token;
    let base = disk_asset_store;

    let idx = PinsIndex::open(&base, cancel).await.unwrap();

    let pins: HashSet<String> = asset_names.iter().map(|s| s.to_string()).collect();
    idx.store(&pins).await.unwrap();

    let loaded = idx.load().await.unwrap();
    assert_eq!(loaded, pins, "pins index must preserve all entries");
}

#[rstest]
#[case(2)]
#[case(3)]
#[case(5)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_concurrent_updates_handled_correctly(
    #[case] asset_count: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let _dir = temp_dir.path();
    let cancel = cancel_token;
    let base = disk_asset_store;

    // Create first index and store some pins
    let idx1 = PinsIndex::open(&base, cancel.clone()).await.unwrap();
    let pins1: HashSet<String> = (0..asset_count)
        .map(|i| format!("asset-{}", i + 1))
        .collect();
    idx1.store(&pins1).await.unwrap();

    // Create second index and load (should see first pins)
    let idx2 = PinsIndex::open(&base, CancellationToken::new())
        .await
        .unwrap();
    let loaded1 = idx2.load().await.unwrap();
    assert_eq!(loaded1, pins1);

    // Update with second index
    let pins2: HashSet<String> = (0..asset_count)
        .map(|i| format!("asset-updated-{}", i + 1))
        .collect();
    idx2.store(&pins2).await.unwrap();

    // First index should see updated pins
    let loaded2 = idx1.load().await.unwrap();
    assert_eq!(loaded2, pins2);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_empty_set_stores_and_loads_correctly(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    disk_asset_store: DiskAssetStore,
) {
    let cancel = cancel_token;
    let base = disk_asset_store;

    let idx = PinsIndex::open(&base, cancel).await.unwrap();

    // Store empty set
    let empty_pins = HashSet::new();
    idx.store(&empty_pins).await.unwrap();

    // Load should return empty set
    let loaded = idx.load().await.unwrap();
    assert!(
        loaded.is_empty(),
        "empty pins set should roundtrip correctly"
    );

    // Verify file exists (may not be created for empty set)
    let path = pins_path(temp_dir.path());
    // File may or may not exist for empty set - both are acceptable
    if path.exists() {
        // If file exists, it should contain empty array
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let pinned = parsed.get("pinned").and_then(|v| v.as_array()).unwrap();
        assert!(
            pinned.is_empty(),
            "pins.json should contain empty array if it exists"
        );
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn pins_index_persists_across_store_instances(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;

    // Create first store and write pins
    let base1 = DiskAssetStore::new(dir);
    let idx1 = PinsIndex::open(&base1, cancel.clone()).await.unwrap();

    let mut pins = HashSet::new();
    pins.insert("persisted-asset".to_string());
    pins.insert("another-asset".to_string());

    idx1.store(&pins).await.unwrap();

    // Create completely new store instance (simulating restart)
    let base2 = DiskAssetStore::new(dir);
    let idx2 = PinsIndex::open(&base2, CancellationToken::new())
        .await
        .unwrap();

    let loaded = idx2.load().await.unwrap();
    assert_eq!(loaded, pins, "pins should persist across store instances");
}
