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

use bytes::Bytes;
use kithara_assets::{AssetStore, AssetStoreBuilder, Assets, EvictConfig, ResourceKey};
use kithara_storage::Resource;
use rstest::{fixture, rstest};
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

fn asset_store_with_root_and_limit(
    temp_dir: &tempfile::TempDir,
    asset_root: &str,
    max_bytes: Option<u64>,
    cancel: CancellationToken,
) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(asset_root)
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes,
        })
        .cancel(cancel)
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
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let dir = temp_dir.path().to_path_buf();

    // Asset A
    {
        let store_a = asset_store_with_root_and_limit(
            &temp_dir,
            asset_a_name,
            Some(100),
            cancel_token.clone(),
        );
        let key_a = ResourceKey::new("media/a.bin");
        let res_a = store_a.open_atomic_resource(&key_a).await.unwrap();

        res_a
            .write(&Bytes::from(vec![0xAAu8; bytes_a]))
            .await
            .unwrap();
        res_a.commit(None).await.unwrap();
    }

    // Wait for async unpinning to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Asset B
    {
        let store_b = asset_store_with_root_and_limit(
            &temp_dir,
            asset_b_name,
            Some(100),
            cancel_token.clone(),
        );
        let key_b = ResourceKey::new("media/b.bin");
        let res_b = store_b.open_atomic_resource(&key_b).await.unwrap();

        res_b
            .write(&Bytes::from(vec![0xBBu8; bytes_b]))
            .await
            .unwrap();
        res_b.commit(None).await.unwrap();
    }

    // Wait for async unpinning to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Asset C: triggers eviction
    {
        let store_c = asset_store_with_root_and_limit(
            &temp_dir,
            asset_c_name,
            Some(100),
            cancel_token.clone(),
        );
        let key_c = ResourceKey::new("media/c.bin");
        let res_c = store_c.open_atomic_resource(&key_c).await.unwrap();

        res_c.write(&Bytes::from_static(b"C")).await.unwrap();
        res_c.commit(None).await.unwrap();
    }

    // Wait for async eviction callback to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Expect A evicted (oldest) to satisfy max_bytes.
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
    let dir = temp_dir.path().to_path_buf();

    // Create assets that approach the limit
    let asset_sizes = [max_bytes / 3, max_bytes / 3];
    let asset_names = vec!["asset-corner-1", "asset-corner-2"];

    for (i, (size, name)) in asset_sizes.iter().zip(asset_names.iter()).enumerate() {
        let store = asset_store_with_root_and_limit(
            &temp_dir,
            name,
            Some(max_bytes as u64),
            cancel.clone(),
        );
        let key = ResourceKey::new(format!("data{}.bin", i));

        let res = store.open_atomic_resource(&key).await.unwrap();
        res.write(&Bytes::from(vec![0x11 * (i + 1) as u8; *size]))
            .await
            .unwrap();
        res.commit(None).await.unwrap();
    }

    // Create a new asset and account its bytes
    {
        let store = asset_store_with_root_and_limit(
            &temp_dir,
            "asset-trigger",
            Some(max_bytes as u64),
            cancel.clone(),
        );
        let trigger_key = ResourceKey::new("trigger.bin");

        let res = store.open_atomic_resource(&trigger_key).await.unwrap();
        res.write(&Bytes::from(vec![0xFF; new_asset_size]))
            .await
            .unwrap();
        res.commit(None).await.unwrap();
    }

    // Trigger eviction by creating another asset_root.
    {
        let store = asset_store_with_root_and_limit(
            &temp_dir,
            "asset-probe",
            Some(max_bytes as u64),
            cancel.clone(),
        );
        let probe_key = ResourceKey::new("probe.bin");
        let probe = store.open_atomic_resource(&probe_key).await.unwrap();
        probe.write(&Bytes::from_static(b"P")).await.unwrap();
        probe.commit(None).await.unwrap();
    }

    assert!(
        exists_asset_dir(&dir, "asset-probe"),
        "asset-probe (newly created) must exist"
    );

    // Eviction behavior: when over limit, oldest assets are evicted until the limit is satisfied.
    // NOTE: asset-trigger is NOT protected - only asset-probe (the newly created asset) is protected.
    let total_old_size: usize = asset_sizes.iter().sum();
    if total_old_size + new_asset_size > max_bytes {
        // When over limit, eviction should free enough space by removing oldest assets.
        let mut evicted_count = 0;
        for name in &asset_names {
            if !exists_asset_dir(&dir, name) {
                evicted_count += 1;
            }
        }
        // Also check if trigger was evicted (it may be needed to satisfy the limit).
        if !exists_asset_dir(&dir, "asset-trigger") {
            evicted_count += 1;
        }

        assert!(
            evicted_count > 0,
            "Should evict at least one asset when over byte limit (total_old={}, new={}, limit={})",
            total_old_size,
            new_asset_size,
            max_bytes
        );
    } else {
        // Under limit - all assets should remain.
        for name in &asset_names {
            assert!(
                exists_asset_dir(&dir, name),
                "{} should remain when under byte limit",
                name
            );
        }
        assert!(
            exists_asset_dir(&dir, "asset-trigger"),
            "asset-trigger should remain when under byte limit"
        );
    }
}
