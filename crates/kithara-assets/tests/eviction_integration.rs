#![forbid(unsafe_code)]

use std::time::Duration;

use bytes::Bytes;
use kithara_assets::{AssetStore, EvictConfig, ResourceKey};
use kithara_storage::Resource;
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

fn exists_asset_dir(root: &std::path::Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

#[allow(dead_code)]
fn read_pins_file(root: &std::path::Path) -> serde_json::Value {
    let path = root.join("_index").join("pins.json");
    let bytes = std::fs::read(&path).expect("pins index file must exist on disk");
    serde_json::from_slice(&bytes).expect("pins index must be valid json")
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
fn asset_store_with_max_2_assets(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: Some(2),
            max_bytes: None,
        },
    )
}

#[rstest]
#[case(2, 3)]
#[case(3, 4)]
#[case(5, 6)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_max_assets_skips_pinned_assets(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let store = AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: Some(max_assets),
            max_bytes: None,
        },
    );

    // Create more assets than the limit, keep the last one pinned
    for i in 0..create_count {
        let key = ResourceKey {
            asset_root: format!("asset-{}", i),
            rel_path: format!("media/{}.bin", i),
        };

        let res = store
            .open_atomic_resource(&key, cancel.clone())
            .await
            .unwrap();
        res.write(&Bytes::from(format!("data-{}", i)))
            .await
            .unwrap();
        res.commit(None).await.unwrap();

        // Keep handle for the last asset to pin it
        if i == create_count - 1 {
            let res_b = res;
            // Sanity: pins file should contain last asset while handle is alive.
            if let Ok(pins_json) = std::fs::read_to_string(dir.join("_index/pins.json")) {
                if let Ok(pins_val) = serde_json::from_str::<serde_json::Value>(&pins_json) {
                    let pinned = pins_val
                        .get("pinned")
                        .and_then(|v| v.as_array())
                        .expect("pins index must contain `pinned` array");
                    assert!(
                        pinned
                            .iter()
                            .any(|v| v.as_str() == Some(&format!("asset-{}", i))),
                        "asset-{} must be pinned while its handle is alive",
                        i
                    );
                }
            }

            // Asset that should trigger eviction - this should remove oldest non-pinned assets
            let key_trigger = ResourceKey {
                asset_root: format!("asset-trigger-{}", i),
                rel_path: "media/trigger.bin".to_string(),
            };
            let res_trigger = store
                .open_atomic_resource(&key_trigger, cancel.clone())
                .await
                .unwrap();
            res_trigger.write(&Bytes::from("trigger")).await.unwrap();
            res_trigger.commit(None).await.unwrap();

            assert!(exists_asset_dir(dir, &format!("asset-trigger-{}", i)));
            assert!(exists_asset_dir(dir, &format!("asset-{}", i))); // pinned asset should remain

            drop(res_b);
        }
    }

    // Count how many asset directories exist
    let mut existing_count = 0;
    for i in 0..=create_count {
        if exists_asset_dir(dir, &format!("asset-{}", i))
            || exists_asset_dir(dir, &format!("asset-trigger-{}", i))
        {
            existing_count += 1;
        }
    }

    // Should have at most max_assets + 1 (trigger asset) remaining
    assert!(
        existing_count <= max_assets + 1,
        "Should have at most {} + 1 assets remaining, got {}",
        max_assets,
        existing_count
    );
}

#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_ignores_missing_index(
    #[case] asset_count: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;

    let store = AssetStore::with_root_dir(
        dir,
        EvictConfig {
            max_assets: Some(2),
            max_bytes: None,
        },
    );

    // Create assets without proper LRU tracking (simulate missing/corrupted index)
    for i in 0..asset_count {
        let key = ResourceKey {
            asset_root: format!("asset-{}", i),
            rel_path: format!("data/{}.bin", i),
        };

        let res = store
            .open_atomic_resource(&key, cancel.clone())
            .await
            .unwrap();
        res.write(&Bytes::from(format!("data-{}", i)))
            .await
            .unwrap();
        res.commit(None).await.unwrap();
    }

    // Manually corrupt LRU index to simulate missing metadata
    let index_path = dir.join("_index/lru.json");
    if index_path.exists() {
        std::fs::write(&index_path, b"corrupted json").unwrap();
    }

    // Creating one more asset should work without crashing despite missing index
    let trigger_key = ResourceKey {
        asset_root: "trigger-asset".to_string(),
        rel_path: "data/trigger.bin".to_string(),
    };

    let res = store
        .open_atomic_resource(&trigger_key, cancel.clone())
        .await;

    assert!(res.is_ok(), "Should handle missing LRU index gracefully");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_with_zero_byte_assets(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;

    let store = AssetStore::with_root_dir(
        dir,
        EvictConfig {
            max_assets: Some(2),
            max_bytes: None,
        },
    );

    // Create assets with zero bytes
    for i in 0..3 {
        let key = ResourceKey {
            asset_root: format!("zero-asset-{}", i),
            rel_path: "empty.bin".to_string(),
        };

        let res = store
            .open_atomic_resource(&key, cancel.clone())
            .await
            .unwrap();
        res.write(&Bytes::new()).await.unwrap();
        res.commit(None).await.unwrap();

        // Explicitly record zero bytes
        store
            .base()
            .touch_asset_bytes(&format!("zero-asset-{}", i), 0, cancel.clone())
            .await
            .unwrap();
    }

    // Should have at most 2 zero-byte assets
    let mut existing_count = 0;
    for i in 0..3 {
        if exists_asset_dir(dir, &format!("zero-asset-{}", i)) {
            existing_count += 1;
        }
    }

    assert!(
        existing_count <= 2,
        "Should have at most 2 zero-byte assets after eviction, got {}",
        existing_count
    );
}

#[rstest]
#[case(1, 2, 3)] // Test different max_assets values
#[case(2, 3, 4)]
#[case(3, 4, 5)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_respects_max_assets_limit(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    #[case] expected_remaining: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;

    let store = AssetStore::with_root_dir(
        dir,
        EvictConfig {
            max_assets: Some(max_assets),
            max_bytes: None,
        },
    );

    // Create more assets than the limit
    let mut handles = Vec::new();
    for i in 0..create_count {
        let key = ResourceKey {
            asset_root: format!("asset-{}", i),
            rel_path: format!("media/{}.bin", i),
        };
        let res = store
            .open_atomic_resource(&key, cancel.clone())
            .await
            .unwrap();
        res.write(&Bytes::from_static(b"DATA")).await.unwrap();
        res.commit(None).await.unwrap();

        // Keep handle for first expected_remaining assets to pin them
        if i < expected_remaining {
            handles.push(res);
        }
    }

    // Count how many asset directories exist
    let mut existing_count = 0;
    for i in 0..create_count {
        if exists_asset_dir(dir, &format!("asset-{}", i)) {
            existing_count += 1;
        }
    }

    // Should have at most max_assets remaining
    assert!(existing_count <= max_assets);

    // First expected_remaining assets should exist (pinned)
    for i in 0..expected_remaining {
        assert!(
            exists_asset_dir(dir, &format!("asset-{}", i)),
            "Asset {} should exist (pinned)",
            i
        );
    }
}
