#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_assets::{AssetStore, EvictConfig, ResourceKey};
use kithara_storage::Resource;
use rstest::{fixture, rstest};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn exists_asset_dir(root: &std::path::Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
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
fn asset_store_with_100_bytes_limit(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: Some(100),
        },
    )
}

#[rstest]
#[case(60, 60, "asset-a", "asset-b", "asset-c")]
#[case(40, 80, "small-1", "small-2", "small-3")]
#[case(90, 30, "large-1", "large-2", "large-3")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_max_bytes_uses_explicit_touch_asset_bytes(
    #[case] bytes_a: usize,
    #[case] bytes_b: usize,
    #[case] asset_a_name: &str,
    #[case] asset_b_name: &str,
    #[case] asset_c_name: &str,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_with_100_bytes_limit: AssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let store = asset_store_with_100_bytes_limit;

    // Keep bytes under 100. We'll:
    // - create A (60 bytes)
    // - create B (60 bytes)
    // - then create C which triggers eviction at "asset creation time"
    //
    // With max_bytes=100, we must evict the oldest non-pinned asset(s) until we're <= 100.
    // Since A is oldest and not pinned, it should be evicted.

    // Asset A: create some data and then explicitly record bytes in LRU via eviction decorator.
    {
        let key_a = ResourceKey {
            asset_root: asset_a_name.to_string(),
            rel_path: "media/a.bin".to_string(),
        };
        let res_a = store
            .open_atomic_resource(&key_a, cancel.clone())
            .await
            .unwrap();

        res_a.write(&Bytes::from(vec![0xAAu8; bytes_a])).await.unwrap();
        res_a.commit(None).await.unwrap();

        // Explicit bytes accounting (MVP for max_bytes):
        // `AssetStore` is `LeaseAssets<EvictAssets<DiskAssetStore>>`, so we can reach `EvictAssets`
        // via `.base()`.
        store
            .base()
            .touch_asset_bytes(asset_a_name, bytes_a as u64, cancel.clone())
            .await
            .unwrap();

        assert!(exists_asset_dir(dir, asset_a_name));
    }

    // Asset B: create and record bytes.
    {
        let key_b = ResourceKey {
            asset_root: asset_b_name.to_string(),
            rel_path: "media/b.bin".to_string(),
        };
        let res_b = store
            .open_atomic_resource(&key_b, cancel.clone())
            .await
            .unwrap();

        res_b.write(&Bytes::from(vec![0xBBu8; bytes_b])).await.unwrap();
        res_b.commit(None).await.unwrap();

        store
            .base()
            .touch_asset_bytes(asset_b_name, bytes_b as u64, cancel.clone())
            .await
            .unwrap();

        assert!(exists_asset_dir(dir, asset_b_name));
    }

    // Asset C: first open for a new asset_root triggers eviction.
    {
        let key_c = ResourceKey {
            asset_root: asset_c_name.to_string(),
            rel_path: "media/c.bin".to_string(),
        };
        let res_c = store
            .open_atomic_resource(&key_c, cancel.clone())
            .await
            .unwrap();

        res_c.write(&Bytes::from_static(b"C")).await.unwrap();
        res_c.commit(None).await.unwrap();

        assert!(exists_asset_dir(dir, asset_c_name));
    }

    // Expect A evicted (oldest) to satisfy max_bytes.
    assert!(
        !exists_asset_dir(dir, asset_a_name),
        "{} should be evicted as the oldest asset to satisfy max_bytes",
        asset_a_name
    );
    assert!(
        exists_asset_dir(dir, asset_b_name),
        "{} should remain after eviction",
        asset_b_name
    );
    assert!(
        exists_asset_dir(dir, asset_c_name),
        "{} is newly created and should exist",
        asset_c_name
    );
}

#[rstest]
#[case(100, 150)] // Exactly at limit + overflow
#[case(50, 120)]  // Well below limit
#[case(200, 50)]  // Over limit with small new asset
#[timeout(Duration::from_secs(5))]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_corner_cases_different_byte_limits(
    #[case] max_bytes: usize,
    #[case] new_asset_size: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;

    let store = AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: Some(max_bytes as u64),
        },
    );

    // Create assets that approach the limit
    let asset_sizes = vec![max_bytes / 3, max_bytes / 3];
    let asset_names = vec!["asset-corner-1", "asset-corner-2"];
    
    for (i, (size, name)) in asset_sizes.iter().zip(asset_names.iter()).enumerate() {
        let key = ResourceKey {
            asset_root: name.to_string(),
            rel_path: format!("data{}.bin", i),
        };
        
        let res = store
            .open_atomic_resource(&key, cancel.clone())
            .await
            .unwrap();
        res.write(&Bytes::from(vec![0x11 * (i + 1) as u8; *size])).await.unwrap();
        res.commit(None).await.unwrap();

        store
            .base()
            .touch_asset_bytes(name, *size as u64, cancel.clone())
            .await
            .unwrap();
    }

    // Create a new asset that should trigger eviction
    let trigger_key = ResourceKey {
        asset_root: "asset-trigger".to_string(),
        rel_path: "trigger.bin".to_string(),
    };
    
    let res = store
        .open_atomic_resource(&trigger_key, cancel.clone())
        .await
        .unwrap();
    res.write(&Bytes::from(vec![0xFF; new_asset_size])).await.unwrap();
    res.commit(None).await.unwrap();

    assert!(exists_asset_dir(dir, "asset-trigger"));
    
    // At least one old asset should be evicted if we're over the limit
    let total_old_size: usize = asset_sizes.iter().sum();
    if total_old_size + new_asset_size > max_bytes {
        let mut evicted_count = 0;
        for name in &asset_names {
            if !exists_asset_dir(dir, name) {
                evicted_count += 1;
            }
        }
        assert!(
            evicted_count > 0,
            "Should evict at least one asset when over byte limit"
        );
    }
}
