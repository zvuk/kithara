use std::{collections::HashSet, num::NonZeroUsize, path::Path, sync::Arc};

use kithara_assets::{
    AssetResourceState, AssetStoreBuilder, EvictConfig, ProcessChunkFn, ResourceKey,
    internal::{DiskAssetStore, PinsIndex, byte_pool},
};
use kithara_platform::time::Duration;
use kithara_storage::ResourceExt;
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

fn load_pins(root_dir: &Path) -> HashSet<String> {
    let disk = DiskAssetStore::new(root_dir, "pins", CancellationToken::new());
    PinsIndex::open(&disk, byte_pool().clone())
        .and_then(|index| index.load())
        .unwrap_or_default()
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_resource_state_is_side_effect_free_and_tracks_multiple_files() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("disk-asset"))
        .build();

    let key_committed = ResourceKey::new("segments/0001.bin");
    let key_failed = ResourceKey::new("segments/0002.bin");
    let committed_path = dir.path().join("disk-asset").join("segments/0001.bin");

    assert_eq!(
        store.resource_state(&key_committed).unwrap(),
        AssetResourceState::Missing
    );
    assert!(
        !committed_path.exists(),
        "resource_state must not create files for missing disk resources"
    );

    let committed = store.acquire_resource(&key_committed).unwrap();
    assert_eq!(
        store.resource_state(&key_committed).unwrap(),
        AssetResourceState::Active
    );

    committed.write_at(0, b"abcd").unwrap();
    committed.commit(Some(4)).unwrap();
    assert_eq!(
        store.resource_state(&key_committed).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) }
    );
    assert!(store.has_resource(&key_committed));

    let failed = store.acquire_resource(&key_failed).unwrap();
    failed.fail("boom".to_string());
    assert_eq!(
        store.resource_state(&key_failed).unwrap(),
        AssetResourceState::Failed("boom".to_string())
    );
    assert!(!store.has_resource(&key_failed));
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn ephemeral_resource_state_tracks_fail_remove_and_lru_eviction() {
    let store = AssetStoreBuilder::new()
        .asset_root(Some("mem-asset"))
        .cache_capacity(NonZeroUsize::new(3).unwrap())
        .ephemeral(true)
        .build();

    let key0 = ResourceKey::new("segments/0000.bin");
    let key1 = ResourceKey::new("segments/0001.bin");
    let key2 = ResourceKey::new("segments/0002.bin");
    let key3 = ResourceKey::new("segments/0003.bin");
    let key_failed = ResourceKey::new("segments/failed.bin");

    assert_eq!(
        store.resource_state(&key0).unwrap(),
        AssetResourceState::Missing
    );

    let res0 = store.acquire_resource(&key0).unwrap();
    assert_eq!(
        store.resource_state(&key0).unwrap(),
        AssetResourceState::Active
    );
    res0.write_at(0, b"zero").unwrap();
    res0.commit(Some(4)).unwrap();
    assert_eq!(
        store.resource_state(&key0).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) }
    );
    assert!(store.has_resource(&key0));

    let failed = store.acquire_resource(&key_failed).unwrap();
    failed.fail("boom".to_string());
    assert_eq!(
        store.resource_state(&key_failed).unwrap(),
        AssetResourceState::Failed("boom".to_string())
    );
    store.remove_resource(&key_failed);
    assert_eq!(
        store.resource_state(&key_failed).unwrap(),
        AssetResourceState::Missing
    );

    for key in [&key1, &key2, &key3] {
        let res = store.acquire_resource(key).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
    }

    assert_eq!(
        store.resource_state(&key0).unwrap(),
        AssetResourceState::Missing
    );
    assert!(!store.has_resource(&key0));
    assert_eq!(
        store.resource_state(&key3).unwrap(),
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

    let store_a = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("asset-a"))
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build();
    let key_a = ResourceKey::new("segments/0001.bin");

    assert_eq!(
        store_a.resource_state(&key_a).unwrap(),
        AssetResourceState::Missing
    );

    let res_a = store_a
        .acquire_resource_with_ctx(&key_a, Some(0x55))
        .unwrap();
    assert_eq!(
        store_a.resource_state(&key_a).unwrap(),
        AssetResourceState::Active
    );
    assert!(
        load_pins(dir.path()).contains("asset-a"),
        "opening a resource must persist a pin while the handle is alive"
    );

    res_a.write_at(0, &[0x10, 0x20, 0x30]).unwrap();
    res_a.commit(Some(3)).unwrap();
    assert_eq!(
        store_a.resource_state(&key_a).unwrap(),
        AssetResourceState::Committed { final_len: Some(3) }
    );

    let reopened = store_a.open_resource(&key_a).unwrap();
    let mut processed = Vec::new();
    reopened.read_into(&mut processed).unwrap();
    assert_eq!(processed, vec![0x45, 0x75, 0x65]);

    let store_b = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("asset-b"))
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build();
    let key_b = ResourceKey::new("segments/0001.bin");
    let res_b = store_b
        .acquire_resource_with_ctx(&key_b, Some(0x11))
        .unwrap();
    res_b.write_at(0, b"bbb").unwrap();
    res_b.commit(Some(3)).unwrap();
    drop(res_b);

    assert_eq!(
        store_a.resource_state(&key_a).unwrap(),
        AssetResourceState::Committed { final_len: Some(3) }
    );

    drop(reopened);
    drop(res_a);
    assert!(
        !load_pins(dir.path()).contains("asset-a"),
        "dropping the last user handle must eagerly unpin even while the store stays alive"
    );

    let store_c = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("asset-c"))
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build();
    let key_c = ResourceKey::new("segments/0001.bin");
    let res_c = store_c
        .acquire_resource_with_ctx(&key_c, Some(0x22))
        .unwrap();
    res_c.write_at(0, b"ccc").unwrap();
    res_c.commit(Some(3)).unwrap();
    drop(res_c);

    let store_a_probe = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("asset-a"))
        .evict_config(evict.clone())
        .process_fn(xor_process_fn())
        .build();
    assert_eq!(
        store_a_probe.resource_state(&key_a).unwrap(),
        AssetResourceState::Missing
    );
    assert_eq!(
        store_b.resource_state(&key_b).unwrap(),
        AssetResourceState::Committed { final_len: Some(3) }
    );
}
