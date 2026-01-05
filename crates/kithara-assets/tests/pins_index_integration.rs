#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara_assets::{AssetStore, DiskAssetStore, EvictConfig, PinsIndex};
use tokio_util::sync::CancellationToken;

fn pins_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("_index").join("pins.json")
}

#[tokio::test]
async fn pins_index_missing_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();

    // `AssetStore` is a leasing decorator, which intentionally does not implement `Assets`.
    // For index tests we open the index against the concrete base store.
    let _store = AssetStore::with_root_dir(
        dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    );
    let base = DiskAssetStore::new(dir.path());

    let path = pins_path(dir.path());
    assert!(!path.exists(), "pins.json must not exist initially");

    let idx = PinsIndex::open(&base, cancel).await.unwrap();
    let pins = idx.load().await.unwrap();

    assert!(
        pins.is_empty(),
        "missing pins index must be treated as empty (best-effort default)"
    );
}

#[tokio::test]
async fn pins_index_invalid_json_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();

    let _store = AssetStore::with_root_dir(
        dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    );
    let base = DiskAssetStore::new(dir.path());

    // Write a corrupted JSON file directly on disk to simulate index damage.
    let index_dir = dir.path().join("_index");
    std::fs::create_dir_all(&index_dir).unwrap();

    let path = pins_path(dir.path());
    std::fs::write(&path, b"{ this is not valid json").unwrap();
    assert!(path.exists(), "pins.json must exist for this test");

    let idx = PinsIndex::open(&base, cancel).await.unwrap();
    let pins = idx.load().await.unwrap();

    assert!(
        pins.is_empty(),
        "invalid JSON pins index must be treated as empty (best-effort default)"
    );
}

#[tokio::test]
async fn pins_index_roundtrip_store_then_load() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();

    let _store = AssetStore::with_root_dir(
        dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    );
    let base = DiskAssetStore::new(dir.path());

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
