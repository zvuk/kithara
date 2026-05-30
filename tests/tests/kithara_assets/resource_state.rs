use std::{collections::HashSet, num::NonZeroUsize, path::Path, sync::Arc};

use kithara_assets::{
    AcquisitionResult, AssetResourceState, AssetStoreBuilder, BytePool, DiskAssetStore,
    EvictConfig, ProcessChunkFn, ReadSide, WriteSide,
};
use kithara_integration_tests::{asset_fixture::PinsIndex, assets_ext::AssetStoreTestExt};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn xor_process_fn() -> ProcessChunkFn<u8> {
    Arc::new(|input, output, ctx: &mut u8, _is_last| {
        for (idx, byte) in input.iter().copied().enumerate() {
            output[idx] = byte ^ *ctx;
        }
        Ok(input.len())
    })
}

fn pending<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>) -> W {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w
}

fn load_pins(root_dir: &Path) -> HashSet<String> {
    let disk = DiskAssetStore::new(root_dir, CancellationToken::new(), &BytePool::default());
    PinsIndex::open(&disk, &BytePool::default())
        .and_then(|index| index.load())
        .unwrap_or_default()
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_resource_state_is_side_effect_free_and_tracks_multiple_files() {
    let dir = tempdir().unwrap();
    let scope = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .build()
        .scope("disk-asset");

    let key_committed = scope.key("segments/0001.bin");
    let key_failed = scope.key("segments/0002.bin");
    let committed_path = dir.path().join("disk-asset").join("segments/0001.bin");

    assert_eq!(
        scope.store().resource_state(&key_committed).unwrap(),
        AssetResourceState::Missing
    );
    assert!(
        !committed_path.exists(),
        "resource_state must not create files for missing disk resources"
    );

    let committed = pending(
        scope
            .store()
            .acquire_resource(&key_committed, None)
            .unwrap(),
    );
    assert_eq!(
        scope.store().resource_state(&key_committed).unwrap(),
        AssetResourceState::Active
    );

    committed.write_at(0, b"abcd").unwrap();
    committed.commit(Some(4)).unwrap();
    assert_eq!(
        scope.store().resource_state(&key_committed).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) }
    );
    assert!(scope.store().has_resource(&key_committed));

    let failed = pending(scope.store().acquire_resource(&key_failed, None).unwrap());
    failed.fail("boom".to_string());
    assert_eq!(
        scope.store().resource_state(&key_failed).unwrap(),
        AssetResourceState::Failed("boom".to_string())
    );
    assert!(!scope.store().has_resource(&key_failed));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_resource_state_keeps_active_status_after_handle_cache_eviction() {
    let dir = tempdir().unwrap();
    let scope = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build()
        .scope("disk-asset");

    let key_active = scope.key("segments/active.bin");
    let key_other = scope.key("segments/other.bin");

    let active = pending(scope.store().acquire_resource(&key_active, None).unwrap());
    active.write_at(0, b"abcd").unwrap();
    assert_eq!(
        scope.store().resource_state(&key_active).unwrap(),
        AssetResourceState::Active
    );

    let other = pending(scope.store().acquire_resource(&key_other, None).unwrap());
    other.write_at(0, b"wxyz").unwrap();

    assert_eq!(
        scope.store().resource_state(&key_active).unwrap(),
        AssetResourceState::Active,
        "live write handle must stay Active even after its cache entry is evicted"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_drop_of_uncommitted_write_handle_does_not_leave_ghost_resource() {
    let dir = tempdir().unwrap();
    let scope = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build()
        .scope("disk-asset");

    let key = scope.key("segments/ghost.bin");
    let handle = pending(scope.store().acquire_resource(&key, None).unwrap());
    handle.write_at(0, b"abcd").unwrap();
    assert_eq!(
        scope.store().resource_state(&key).unwrap(),
        AssetResourceState::Active
    );

    drop(handle);

    assert_eq!(
        scope.store().resource_state(&key).unwrap(),
        AssetResourceState::Missing,
        "abandoned writes must not surface as committed ghost files after the handle drops"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_open_resource_on_missing_key_does_not_create_ghost_file() {
    let dir = tempdir().unwrap();
    let scope = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build()
        .scope("disk-asset");

    let key = scope.key("segments/missing.bin");
    let path = dir.path().join("disk-asset").join("segments/missing.bin");

    assert_eq!(
        scope.store().resource_state(&key).unwrap(),
        AssetResourceState::Missing
    );
    assert!(!path.exists());

    let err = scope
        .store()
        .open_resource(&key, None)
        .expect_err("read-open on a missing resource must not create it");
    assert!(
        err.to_string().contains("No such file")
            || err.to_string().contains("not found")
            || err.to_string().contains("missing"),
        "missing read-open should report absence, got: {err}"
    );

    assert_eq!(
        scope.store().resource_state(&key).unwrap(),
        AssetResourceState::Missing
    );
    assert!(
        !path.exists(),
        "read-open on a missing disk resource must not leave a zero-filled ghost file"
    );
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn ephemeral_resource_state_tracks_fail_remove_and_lru_eviction() {
    let scope = AssetStoreBuilder::new()
        .cache_capacity(NonZeroUsize::new(3).unwrap())
        .ephemeral(true)
        .build()
        .scope("mem-asset");

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let key2 = scope.key("segments/0002.bin");
    let key3 = scope.key("segments/0003.bin");
    let key_failed = scope.key("segments/failed.bin");

    assert_eq!(
        scope.store().resource_state(&key0).unwrap(),
        AssetResourceState::Missing
    );

    let res0_writer = pending(scope.store().acquire_resource(&key0, None).unwrap());
    assert_eq!(
        scope.store().resource_state(&key0).unwrap(),
        AssetResourceState::Active
    );
    res0_writer.write_at(0, b"zero").unwrap();
    let res0 = res0_writer.commit(Some(4)).unwrap();
    assert_eq!(
        scope.store().resource_state(&key0).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) }
    );
    assert!(scope.store().has_resource(&key0));

    let failed = pending(scope.store().acquire_resource(&key_failed, None).unwrap());
    failed.fail("boom".to_string());
    assert_eq!(
        scope.store().resource_state(&key_failed).unwrap(),
        AssetResourceState::Failed("boom".to_string())
    );
    scope.store().remove_resource(&key_failed);
    assert_eq!(
        scope.store().resource_state(&key_failed).unwrap(),
        AssetResourceState::Missing
    );

    for key in [&key1, &key2, &key3] {
        let res = pending(scope.store().acquire_resource(key, None).unwrap());
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
    }

    assert_eq!(
        scope.store().resource_state(&key0).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) },
        "live user handle must keep the resource readable even after LRU eviction"
    );

    drop(res0);

    assert_eq!(
        scope.store().resource_state(&key0).unwrap(),
        AssetResourceState::Missing
    );
    assert!(!scope.store().has_resource(&key0));
    assert_eq!(
        scope.store().resource_state(&key3).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) }
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_resource_state_tracks_processing_pins_and_asset_eviction() {
    let dir = tempdir().unwrap();
    let evict = EvictConfig {
        max_assets: Some(2),
        max_bytes: None,
    };

    let scope_a = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build()
        .scope("asset-a");
    let key_a = scope_a.key("segments/0001.bin");

    assert_eq!(
        scope_a.store().resource_state(&key_a).unwrap(),
        AssetResourceState::Missing
    );

    let res_a_writer = pending(
        scope_a
            .store()
            .acquire_resource_with_ctx(&key_a, None, Some(0x55))
            .unwrap(),
    );
    assert_eq!(
        scope_a.store().resource_state(&key_a).unwrap(),
        AssetResourceState::Active
    );
    assert!(
        load_pins(dir.path()).contains("asset-a"),
        "opening a resource must persist a pin while the handle is alive"
    );

    res_a_writer.write_at(0, &[0x10, 0x20, 0x30]).unwrap();
    let res_a = res_a_writer.commit(Some(3)).unwrap();
    assert_eq!(
        scope_a.store().resource_state(&key_a).unwrap(),
        AssetResourceState::Committed { final_len: Some(3) }
    );

    let reopened = scope_a.store().open_resource(&key_a, None).unwrap();
    let mut processed = Vec::new();
    reopened.read_into(&mut processed).unwrap();
    assert_eq!(processed, vec![0x45, 0x75, 0x65]);

    let scope_b = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build()
        .scope("asset-b");
    let key_b = scope_b.key("segments/0001.bin");
    let res_b = pending(
        scope_b
            .store()
            .acquire_resource_with_ctx(&key_b, None, Some(0x11))
            .unwrap(),
    );
    res_b.write_at(0, b"bbb").unwrap();
    let res_b = res_b.commit(Some(3)).unwrap();
    drop(res_b);

    assert_eq!(
        scope_a.store().resource_state(&key_a).unwrap(),
        AssetResourceState::Committed { final_len: Some(3) }
    );

    drop(reopened);
    drop(res_a);
    assert!(
        !load_pins(dir.path()).contains("asset-a"),
        "dropping the last user handle must eagerly unpin even while the store stays alive"
    );

    let scope_c = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build()
        .scope("asset-c");
    let key_c = scope_c.key("segments/0001.bin");
    let res_c = pending(
        scope_c
            .store()
            .acquire_resource_with_ctx(&key_c, None, Some(0x22))
            .unwrap(),
    );
    res_c.write_at(0, b"ccc").unwrap();
    let res_c = res_c.commit(Some(3)).unwrap();
    drop(res_c);

    let scope_a_probe = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build()
        .scope("asset-a");
    assert_eq!(
        scope_a_probe.store().resource_state(&key_a).unwrap(),
        AssetResourceState::Missing
    );
    assert_eq!(
        scope_b.store().resource_state(&key_b).unwrap(),
        AssetResourceState::Committed { final_len: Some(3) }
    );
}
