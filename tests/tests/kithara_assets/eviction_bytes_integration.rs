#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use bytes::Bytes;
use kithara::assets::{AssetScope, AssetStoreBuilder, EvictConfig, ResourceHandle};
use kithara_integration_tests::{cancel_token, temp_dir};
use kithara_platform::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

#[cfg(not(target_arch = "wasm32"))]
fn exists_asset_dir(root: &Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

#[cfg(not(target_arch = "wasm32"))]
fn asset_scope_with_root_and_limit(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
    max_bytes: Option<u64>,
    cancel: CancellationToken,
) -> AssetScope {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes,
        })
        .cancel(cancel)
        .build()
        .scope(asset_root)
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(60, 60, "asset-a", "asset-b", "asset-c")]
#[case(40, 80, "small-1", "small-2", "small-3")]
#[case(90, 30, "large-1", "large-2", "large-3")]
async fn eviction_max_bytes_uses_explicit_touch_asset_bytes(
    #[case] bytes_a: usize,
    #[case] bytes_b: usize,
    #[case] asset_a_name: &str,
    #[case] asset_b_name: &str,
    #[case] asset_c_name: &str,
    cancel_token: CancellationToken,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let dir = temp_dir.path().to_path_buf();

    {
        let scope_a = asset_scope_with_root_and_limit(
            &temp_dir,
            asset_a_name,
            Some(100),
            cancel_token.clone(),
        );
        let key_a = scope_a.key("media/a.bin");
        let res_a = scope_a.store().acquire_resource(&key_a, None).unwrap();

        res_a
            .write_all(&Bytes::from(vec![0xAAu8; bytes_a]))
            .unwrap();
    }

    sleep(Duration::from_millis(50)).await;

    {
        let scope_b = asset_scope_with_root_and_limit(
            &temp_dir,
            asset_b_name,
            Some(100),
            cancel_token.clone(),
        );
        let key_b = scope_b.key("media/b.bin");
        let res_b = scope_b.store().acquire_resource(&key_b, None).unwrap();

        res_b
            .write_all(&Bytes::from(vec![0xBBu8; bytes_b]))
            .unwrap();
    }

    sleep(Duration::from_millis(50)).await;

    {
        let scope_c = asset_scope_with_root_and_limit(
            &temp_dir,
            asset_c_name,
            Some(100),
            cancel_token.clone(),
        );
        let key_c = scope_c.key("media/c.bin");
        let res_c = scope_c.store().acquire_resource(&key_c, None).unwrap();

        res_c.write_all(b"C").unwrap();
    }

    sleep(Duration::from_millis(100)).await;

    let asset_a_path = dir.join(asset_a_name).join("media/a.bin");
    assert!(
        !asset_a_path.exists(),
        "{} should be evicted as the oldest asset to satisfy max_bytes. Path: {:?}",
        asset_a_name,
        asset_a_path
    );

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

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(100, 150)]
#[case(50, 120)]
#[case(200, 50)]
fn eviction_corner_cases_different_byte_limits(
    #[case] max_bytes: usize,
    #[case] new_asset_size: usize,
    cancel_token: CancellationToken,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let cancel = cancel_token;
    let dir = temp_dir.path().to_path_buf();

    let asset_sizes = [max_bytes / 3, max_bytes / 3];
    let asset_names = vec!["asset-corner-1", "asset-corner-2"];

    for (i, (size, name)) in asset_sizes.iter().zip(asset_names.iter()).enumerate() {
        let scope = asset_scope_with_root_and_limit(
            &temp_dir,
            name,
            Some(max_bytes as u64),
            cancel.clone(),
        );
        let key = scope.key(format!("data{}.bin", i));

        let res = scope.store().acquire_resource(&key, None).unwrap();
        res.write_all(&Bytes::from(vec![0x11 * (i + 1) as u8; *size]))
            .unwrap();
    }

    {
        let scope = asset_scope_with_root_and_limit(
            &temp_dir,
            "asset-trigger",
            Some(max_bytes as u64),
            cancel.clone(),
        );
        let trigger_key = scope.key("trigger.bin");

        let res = scope.store().acquire_resource(&trigger_key, None).unwrap();
        res.write_all(&Bytes::from(vec![0xFF; new_asset_size]))
            .unwrap();
    }

    {
        let scope = asset_scope_with_root_and_limit(
            &temp_dir,
            "asset-probe",
            Some(max_bytes as u64),
            cancel.clone(),
        );
        let probe_key = scope.key("probe.bin");
        let probe = scope.store().acquire_resource(&probe_key, None).unwrap();
        probe.write_all(b"P").unwrap();
    }

    assert!(
        exists_asset_dir(&dir, "asset-probe"),
        "asset-probe (newly created) must exist"
    );

    // NOTE: asset-trigger is NOT protected - only asset-probe (the newly created asset) is protected.
    let total_old_size: usize = asset_sizes.iter().sum();
    if total_old_size + new_asset_size > max_bytes {
        let mut evicted_count = 0;
        for name in &asset_names {
            if !exists_asset_dir(&dir, name) {
                evicted_count += 1;
            }
        }
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
