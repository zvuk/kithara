#![forbid(unsafe_code)]

use std::time::Duration;

use bytes::Bytes;
use kithara_assets::{AssetStore, AssetStoreBuilder, Assets, EvictConfig, ResourceKey};
use kithara_storage::Resource;
use rstest::{fixture, rstest};
use tokio::task::yield_now;
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
fn asset_store_with_100_bytes_limit(
    temp_dir: tempfile::TempDir,
    cancel_token: CancellationToken,
) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: Some(100),
        })
        .cancel(cancel_token.clone())
        .build()
}

#[rstest]
#[case(60, 60, "asset-a", "asset-b", "asset-c")]
#[case(40, 80, "small-1", "small-2", "small-3")]
#[case(90, 30, "large-1", "large-2", "large-3")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn eviction_max_bytes_uses_explicit_touch_asset_bytes(
    #[case] bytes_a: usize,
    #[case] bytes_b: usize,
    #[case] asset_a_name: &str,
    #[case] asset_b_name: &str,
    #[case] asset_c_name: &str,
    _cancel_token: CancellationToken,
    asset_store_with_100_bytes_limit: AssetStore,
) {
    let store = asset_store_with_100_bytes_limit;
    let evict = store.base().clone();

    // Get the root directory from the store
    let dir = store.root_dir();

    // Keep bytes under 100. We'll:
    // - create A (60 bytes)
    // - create B (60 bytes)
    // - then create C which triggers eviction at "asset creation time"
    //
    // With max_bytes=100, we must evict the oldest non-pinned asset(s) until we're <= 100.
    // Since A is oldest and not pinned, it should be evicted.

    // Asset A: create some data and then explicitly record bytes in LRU via eviction decorator.
    {
        let key_a = ResourceKey::new(asset_a_name.to_string(), "media/a.bin".to_string());
        let res_a = evict.open_atomic_resource(&key_a).await.unwrap();

        res_a
            .write(&Bytes::from(vec![0xAAu8; bytes_a]))
            .await
            .unwrap();
        res_a.commit(None).await.unwrap();

        // Explicit bytes accounting (MVP for max_bytes):
        // `AssetStore` is `LeaseAssets<EvictAssets<DiskAssetStore>>`, so we can reach `EvictAssets`
        // via `.base()`.
        evict
            .touch_asset_bytes(asset_a_name, bytes_a as u64)
            .await
            .unwrap();

        // Check that the resource file exists at the correct path
        let expected_path = dir.join(asset_a_name).join("media/a.bin");
        assert_eq!(res_a.path(), expected_path);
        assert!(
            res_a.path().exists(),
            "Resource file should exist at {:?}",
            res_a.path()
        );
    }

    // Allow background unpin to run before the next asset creation.
    yield_now().await;

    // Asset B: create and record bytes.
    {
        let key_b = ResourceKey::new(asset_b_name.to_string(), "media/b.bin".to_string());
        let res_b = evict.open_atomic_resource(&key_b).await.unwrap();

        res_b
            .write(&Bytes::from(vec![0xBBu8; bytes_b]))
            .await
            .unwrap();
        res_b.commit(None).await.unwrap();

        evict
            .touch_asset_bytes(asset_b_name, bytes_b as u64)
            .await
            .unwrap();

        // Check that the resource file exists at the correct path
        let expected_path = dir.join(asset_b_name).join("media/b.bin");
        assert_eq!(res_b.path(), expected_path);
        assert!(
            res_b.path().exists(),
            "Resource file should exist at {:?}",
            res_b.path()
        );
    }

    // Allow background unpin to run before the next asset creation.
    yield_now().await;

    // Asset C: first open for a new asset_root triggers eviction.
    {
        let key_c = ResourceKey::new(asset_c_name.to_string(), "media/c.bin".to_string());
        let res_c = evict.open_atomic_resource(&key_c).await.unwrap();

        res_c.write(&Bytes::from_static(b"C")).await.unwrap();
        res_c.commit(None).await.unwrap();

        // Check that the resource file exists at the correct path
        let expected_path = dir.join(asset_c_name).join("media/c.bin");
        assert_eq!(res_c.path(), expected_path);
        assert!(
            res_c.path().exists(),
            "Resource file should exist at {:?}",
            res_c.path()
        );
    }

    // Expect A evicted (oldest) to satisfy max_bytes.
    // Check that the resource file no longer exists
    let asset_a_path = dir.join(asset_a_name).join("media/a.bin");
    assert!(
        !asset_a_path.exists(),
        "{} should be evicted as the oldest asset to satisfy max_bytes. Path: {:?}",
        asset_a_name,
        asset_a_path
    );

    // Check that asset B and C still exist
    let asset_b_path = dir.join(asset_b_name).join("media/b.bin");
    assert!(
        asset_b_path.exists(),
        "{} should remain after eviction. Path: {:?}",
        asset_b_name,
        asset_b_path
    );

    let asset_c_path = dir.join(asset_c_name).join("media/c.bin");
    assert!(
        asset_c_path.exists(),
        "{} is newly created and should exist. Path: {:?}",
        asset_c_name,
        asset_c_path
    );
}

#[rstest]
#[case(100, 150)] // Exactly at limit + overflow
#[case(50, 120)] // Well below limit
#[case(200, 50)] // Over limit with small new asset
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn eviction_corner_cases_different_byte_limits(
    #[case] max_bytes: usize,
    #[case] new_asset_size: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let cancel = cancel_token;

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: Some(max_bytes as u64),
        })
        .cancel(cancel.clone())
        .build();

    let dir = store.root_dir();
    let evict = store.base().clone();

    // Create assets that approach the limit
    let asset_sizes = [max_bytes / 3, max_bytes / 3];
    let asset_names = vec!["asset-corner-1", "asset-corner-2"];

    for (i, (size, name)) in asset_sizes.iter().zip(asset_names.iter()).enumerate() {
        let key = ResourceKey::new(name.to_string(), format!("data{}.bin", i));

        let res = evict.open_atomic_resource(&key).await.unwrap();
        res.write(&Bytes::from(vec![0x11 * (i + 1) as u8; *size]))
            .await
            .unwrap();
        res.commit(None).await.unwrap();

        evict.touch_asset_bytes(name, *size as u64).await.unwrap();
    }

    // Create a new asset and account its bytes
    let trigger_key = ResourceKey::new("asset-trigger".to_string(), "trigger.bin".to_string());

    let res = evict.open_atomic_resource(&trigger_key).await.unwrap();
    res.write(&Bytes::from(vec![0xFF; new_asset_size]))
        .await
        .unwrap();
    res.commit(None).await.unwrap();

    evict
        .touch_asset_bytes("asset-trigger", new_asset_size as u64)
        .await
        .unwrap();

    assert!(exists_asset_dir(dir, "asset-trigger"));

    // Trigger eviction after accounting the trigger bytes by creating another asset_root.
    let probe_key = ResourceKey::new("asset-probe".to_string(), "probe.bin".to_string());
    let probe = evict.open_atomic_resource(&probe_key).await.unwrap();
    probe.write(&Bytes::from_static(b"P")).await.unwrap();
    probe.commit(None).await.unwrap();

    assert!(exists_asset_dir(dir, "asset-probe"));

    // At least one old asset should be evicted if we're over the limit; otherwise they remain.
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
            "Should evict at least one older asset when over byte limit"
        );
    } else {
        for name in &asset_names {
            assert!(
                exists_asset_dir(dir, name),
                "{} should remain when under byte limit",
                name
            );
        }
    }
}
