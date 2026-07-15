#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::Path};

use kithara::{
    assets::{
        AcquisitionResult, AssetScope, AssetStoreBuilder, StorageBackend, WriteSide,
        index::schema::{ArchivedPinsIndexFile, PinsIndexFile},
    },
    platform::{thread, time::Duration},
};
use kithara_integration_tests::temp_dir;

use super::support::{AssetScopeTestKeyExt, AssetStoreTestScopeExt, literal_layouts};

#[cfg(not(target_arch = "wasm32"))]
fn exists_asset_dir(root: &Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

#[cfg(not(target_arch = "wasm32"))]
fn pending<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>) -> W {
    let AcquisitionResult::Pending(writer) = acq else {
        panic!("fresh acquire must be Pending");
    };
    writer
}

#[cfg(not(target_arch = "wasm32"))]
fn asset_scope_with_root(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
    max_assets: Option<usize>,
) -> AssetScope {
    AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (temp_dir.path()).into(),
        })
        .maybe_max_assets(max_assets)
        .layouts(literal_layouts())
        .build()
        .test_scope(asset_root)
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(2, 3)]
#[case(3, 4)]
#[case(5, 6)]
fn eviction_max_assets_skips_pinned_assets(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let dir = temp_dir.path().to_path_buf();

    for i in 0..create_count {
        let asset_root = format!("asset-{}", i);
        let scope = asset_scope_with_root(&temp_dir, &asset_root, Some(max_assets));
        let key = scope.test_key(format!("media/{}.bin", i));

        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        let data = format!("data-{}", i);
        writer.write_at(0, data.as_bytes()).unwrap();
        let res = writer.commit(Some(data.len() as u64)).unwrap();

        if i == create_count - 1 {
            let res_b = res;
            if let Ok(pins_bytes) = fs::read(dir.join("_index/pins.bin"))
                && let Ok(archived) =
                    rkyv::access::<ArchivedPinsIndexFile, rkyv::rancor::Error>(&pins_bytes)
            {
                let pins_file: PinsIndexFile =
                    rkyv::deserialize::<PinsIndexFile, rkyv::rancor::Error>(archived).unwrap();
                let mut is_pinned = false;
                for (k, v) in &pins_file.pinned {
                    if *v && k.as_str() == format!("asset-{}", i) {
                        is_pinned = true;
                        break;
                    }
                }
                assert!(
                    is_pinned,
                    "asset-{} must be pinned while its handle is alive",
                    i
                );
            }

            let trigger_root = format!("asset-trigger-{}", i);
            let trigger_scope = asset_scope_with_root(&temp_dir, &trigger_root, Some(max_assets));
            let key_trigger = trigger_scope.test_key("media/trigger.bin");
            let writer_trigger = pending(
                trigger_scope
                    .store()
                    .acquire_resource(&key_trigger, None)
                    .unwrap(),
            );
            writer_trigger.write_at(0, b"trigger").unwrap();
            let _res_trigger = writer_trigger
                .commit(Some(b"trigger".len() as u64))
                .unwrap();

            assert!(exists_asset_dir(&dir, &format!("asset-trigger-{}", i)));
            assert!(exists_asset_dir(&dir, &format!("asset-{}", i)));

            drop(res_b);
        }
    }

    let mut existing_count = 0;
    for i in 0..=create_count {
        if exists_asset_dir(&dir, &format!("asset-{}", i))
            || exists_asset_dir(&dir, &format!("asset-trigger-{}", i))
        {
            existing_count += 1;
        }
    }

    assert!(
        existing_count <= max_assets + 1,
        "Should have at most {} + 1 assets remaining, got {}",
        max_assets,
        existing_count
    );
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(1)]
#[case(2)]
#[case(3)]
fn eviction_ignores_missing_index(
    #[case] asset_count: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let dir = temp_dir.path().to_path_buf();

    for i in 0..asset_count {
        let asset_root = format!("asset-{}", i);
        let scope = asset_scope_with_root(&temp_dir, &asset_root, Some(2));
        let key = scope.test_key(format!("data/{}.bin", i));

        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        let data = format!("data-{}", i);
        writer.write_at(0, data.as_bytes()).unwrap();
        let _res = writer.commit(Some(data.len() as u64)).unwrap();
    }

    let index_path = dir.join("_index/lru.bin");
    if index_path.exists() {
        fs::write(&index_path, b"corrupted json").unwrap();
    }

    let trigger_scope = asset_scope_with_root(&temp_dir, "trigger-asset", Some(2));
    let trigger_key = trigger_scope.test_key("data/trigger.bin");

    let res = trigger_scope.store().acquire_resource(&trigger_key, None);

    assert!(res.is_ok(), "Should handle missing LRU index gracefully");
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn eviction_with_zero_byte_assets(temp_dir: kithara_integration_tests::TestTempDir) {
    let dir = temp_dir.path().to_path_buf();

    for i in 0..3 {
        let asset_root = format!("zero-asset-{}", i);
        let scope = asset_scope_with_root(&temp_dir, &asset_root, Some(2));
        let key = scope.test_key("empty.bin");

        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        writer.write_at(0, b"").unwrap();
        let _res = writer.commit(Some(0)).unwrap();
    }

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

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(1, 3, 1)]
#[case(2, 4, 1)]
#[case(3, 6, 2)]
fn eviction_respects_max_assets_limit(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    #[case] pinned_count: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let dir = temp_dir.path().to_path_buf();

    let mut pinned_handles = Vec::new();
    for i in 0..create_count {
        let asset_root = format!("asset-{}", i);
        let scope = asset_scope_with_root(&temp_dir, &asset_root, Some(max_assets));
        let key = scope.test_key(format!("media/{}.bin", i));
        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        writer.write_at(0, b"DATA").unwrap();
        let res = writer.commit(Some(b"DATA".len() as u64)).unwrap();

        if i >= create_count - pinned_count {
            pinned_handles.push((scope, res));
        }
    }

    thread::sleep(Duration::from_millis(100));

    let mut existing_count = 0;
    for i in 0..create_count {
        if exists_asset_dir(&dir, &format!("asset-{}", i)) {
            existing_count += 1;
        }
    }

    assert!(
        existing_count <= max_assets + pinned_count,
        "existing_count={} should be <= max_assets={} + pinned_count={} = {}",
        existing_count,
        max_assets,
        pinned_count,
        max_assets + pinned_count
    );

    for i in create_count - pinned_count..create_count {
        assert!(
            exists_asset_dir(&dir, &format!("asset-{}", i)),
            "Pinned asset {} should exist",
            i
        );
    }

    let non_pinned_remaining = existing_count.saturating_sub(pinned_count);
    assert!(
        non_pinned_remaining <= max_assets,
        "non_pinned_remaining={} should be <= max_assets={}",
        non_pinned_remaining,
        max_assets
    );
}
