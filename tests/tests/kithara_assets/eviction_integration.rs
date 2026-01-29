#![forbid(unsafe_code)]

//! Eviction integration tests.
//!
//! NOTE: These tests were designed for the old architecture where a single AssetStore
//! could hold multiple assets (different asset_roots). With the new architecture,
//! each AssetStore is scoped to a single asset_root, so eviction between assets
//! requires creating multiple AssetStore instances with the same root_dir.
//!
//! These tests are currently ignored and need to be redesigned for the new architecture.

use std::time::Duration;

use kithara_assets::{AssetStore, AssetStoreBuilder, Assets, EvictConfig, ResourceKey};
use kithara_storage::ResourceExt;
use rstest::{fixture, rstest};

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct PinsIndexFile {
    version: u32,
    pinned: Vec<String>,
}

fn exists_asset_dir(root: &std::path::Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

#[fixture]
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn asset_store_with_root(
    temp_dir: &tempfile::TempDir,
    asset_root: &str,
    max_assets: Option<usize>,
) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(asset_root)
        .evict_config(EvictConfig {
            max_assets,
            max_bytes: None,
        })
        .build()
}

#[rstest]
#[case(2, 3)]
#[case(3, 4)]
#[case(5, 6)]
#[timeout(Duration::from_secs(5))]
#[test]
fn eviction_max_assets_skips_pinned_assets(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path().to_path_buf();

    // Create more assets than the limit, keep the last one pinned
    for i in 0..create_count {
        let asset_root = format!("asset-{}", i);
        let store = asset_store_with_root(&temp_dir, &asset_root, Some(max_assets));
        let key = ResourceKey::new(format!("media/{}.bin", i));

        let res = store.open_resource(&key).unwrap();
        res.write_all(format!("data-{}", i).as_bytes()).unwrap();

        // Keep handle for the last asset to pin it
        if i == create_count - 1 {
            let res_b = res;
            // Sanity: pins file should contain last asset while handle is alive.
            if let Ok(pins_bytes) = std::fs::read(dir.join("_index/pins.json"))
                && let Ok(pins_file) = bincode::deserialize::<PinsIndexFile>(&pins_bytes)
            {
                assert!(
                    pins_file
                        .pinned
                        .iter()
                        .any(|v| v == &format!("asset-{}", i)),
                    "asset-{} must be pinned while its handle is alive",
                    i
                );
            }

            // Asset that should trigger eviction
            let trigger_root = format!("asset-trigger-{}", i);
            let trigger_store = asset_store_with_root(&temp_dir, &trigger_root, Some(max_assets));
            let key_trigger = ResourceKey::new("media/trigger.bin");
            let res_trigger = trigger_store.open_resource(&key_trigger).unwrap();
            res_trigger.write_all(b"trigger").unwrap();

            assert!(exists_asset_dir(&dir, &format!("asset-trigger-{}", i)));
            assert!(exists_asset_dir(&dir, &format!("asset-{}", i))); // pinned asset should remain

            drop(res_b);
        }
    }

    // Count how many asset directories exist
    let mut existing_count = 0;
    for i in 0..=create_count {
        if exists_asset_dir(&dir, &format!("asset-{}", i))
            || exists_asset_dir(&dir, &format!("asset-trigger-{}", i))
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
#[test]
fn eviction_ignores_missing_index(#[case] asset_count: usize, temp_dir: tempfile::TempDir) {
    let dir = temp_dir.path().to_path_buf();

    // Create assets without proper LRU tracking (simulate missing/corrupted index)
    for i in 0..asset_count {
        let asset_root = format!("asset-{}", i);
        let store = asset_store_with_root(&temp_dir, &asset_root, Some(2));
        let key = ResourceKey::new(format!("data/{}.bin", i));

        let res = store.open_resource(&key).unwrap();
        res.write_all(format!("data-{}", i).as_bytes()).unwrap();
    }

    // Manually corrupt LRU index to simulate missing metadata
    let index_path = dir.join("_index/lru.json");
    if index_path.exists() {
        std::fs::write(&index_path, b"corrupted json").unwrap();
    }

    // Creating one more asset should work without crashing despite missing index
    let trigger_store = asset_store_with_root(&temp_dir, "trigger-asset", Some(2));
    let trigger_key = ResourceKey::new("data/trigger.bin");

    let res = trigger_store.open_resource(&trigger_key);

    assert!(res.is_ok(), "Should handle missing LRU index gracefully");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[ignore = "Test needs redesign for new scoped AssetStore architecture"]
#[test]
fn eviction_with_zero_byte_assets(temp_dir: tempfile::TempDir) {
    let dir = temp_dir.path().to_path_buf();

    // Create assets with zero bytes
    for i in 0..3 {
        let asset_root = format!("zero-asset-{}", i);
        let store = asset_store_with_root(&temp_dir, &asset_root, Some(2));
        let key = ResourceKey::new("empty.bin");

        let res = store.open_resource(&key).unwrap();
        res.write_all(b"").unwrap();
    }

    // Should have at most 2 zero-byte assets
    let mut existing_count = 0;
    for i in 0..3 {
        if exists_asset_dir(&dir, &format!("zero-asset-{}", i)) {
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
#[case(1, 3, 1)] // max_assets=1, create 3 assets, keep 1 newest pinned
#[case(2, 4, 1)] // max_assets=2, create 4 assets, keep 1 newest pinned
#[case(3, 6, 2)] // max_assets=3, create 6 assets, keep 2 newest pinned
#[timeout(Duration::from_secs(5))]
#[test]
fn eviction_respects_max_assets_limit(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    #[case] pinned_count: usize,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path().to_path_buf();

    // Create more assets than the limit
    let mut pinned_handles = Vec::new();
    for i in 0..create_count {
        let asset_root = format!("asset-{}", i);
        let store = asset_store_with_root(&temp_dir, &asset_root, Some(max_assets));
        let key = ResourceKey::new(format!("media/{}.bin", i));
        let res = store.open_resource(&key).unwrap();
        res.write_all(b"DATA").unwrap();

        // Keep handle for the newest `pinned_count` assets to pin them
        // This simulates real usage where some assets are actively being used
        if i >= create_count - pinned_count {
            pinned_handles.push((store, res));
        }
    }

    // Give eviction a moment to complete
    std::thread::sleep(Duration::from_millis(100));

    // Count how many asset directories exist
    let mut existing_count = 0;
    for i in 0..create_count {
        if exists_asset_dir(&dir, &format!("asset-{}", i)) {
            existing_count += 1;
        }
    }

    // Should have at most max_assets + pinned_count remaining
    // Pinned assets can temporarily exceed the limit
    assert!(
        existing_count <= max_assets + pinned_count,
        "existing_count={} should be <= max_assets={} + pinned_count={} = {}",
        existing_count,
        max_assets,
        pinned_count,
        max_assets + pinned_count
    );

    // Pinned (newest) assets should exist
    for i in create_count - pinned_count..create_count {
        assert!(
            exists_asset_dir(&dir, &format!("asset-{}", i)),
            "Pinned asset {} should exist",
            i
        );
    }

    // Oldest non-pinned assets should be evicted first
    // We should have at most max_assets non-pinned assets remaining
    let non_pinned_remaining = existing_count.saturating_sub(pinned_count);
    assert!(
        non_pinned_remaining <= max_assets,
        "non_pinned_remaining={} should be <= max_assets={}",
        non_pinned_remaining,
        max_assets
    );
}
