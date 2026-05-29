//! Phase P-4 integration tests for `AssetStore::checkpoint`.

#![cfg(not(target_arch = "wasm32"))]

use kithara_assets::{AssetStoreBuilder, ResourceHandle};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use tempfile::tempdir;

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_persists_committed_resource_across_rebuild() {
    let dir = tempdir().unwrap();
    let root = "p4-persist";

    {
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
        let scope = store.scope(root);
        let key = scope.key("segments/0001.bin");
        let res = scope.store().acquire_resource(&key, None).unwrap();
        res.write_at(0, b"hello world").unwrap();
        res.commit(Some(11)).unwrap();
        drop(res);
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
    let scope = store.scope(root);
    let key = scope.key("segments/0001.bin");

    assert_eq!(scope.store().final_len(&key), Some(11));
    assert!(scope.store().contains_range(&key, 0..11));

    let ranges = scope.store().available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(pairs, vec![(0, 11)]);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_drops_partial_writes_when_writer_abandons_without_commit() {
    let dir = tempdir().unwrap();
    let root = "p4-partial";

    {
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
        let scope = store.scope(root);
        let key = scope.key("segments/partial.bin");
        let res = scope.store().acquire_resource(&key, None).unwrap();
        res.write_at(0, b"aaa").unwrap();
        res.write_at(10, b"bbb").unwrap();
        drop(res);
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
    let scope = store.scope(root);
    let key = scope.key("segments/partial.bin");

    let ranges = scope.store().available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert!(
        pairs.is_empty(),
        "writer abandoned without commit must leave availability empty, got {pairs:?}"
    );
    assert!(!scope.store().contains_range(&key, 0..3));
    assert!(!scope.store().contains_range(&key, 10..13));
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_without_prior_writes_is_noop() {
    let dir = tempdir().unwrap();
    let root = "p4-empty";
    let store = AssetStoreBuilder::new().root_dir(dir.path()).build();

    store.checkpoint().unwrap();

    let store2 = AssetStoreBuilder::new().root_dir(dir.path()).build();
    let scope2 = store2.scope(root);
    let key = scope2.key("ghost.bin");

    assert!(scope2.store().available_ranges(&key).is_empty());
    assert_eq!(scope2.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_rebuild_without_checkpoint_falls_back_to_slow_path() {
    let dir = tempdir().unwrap();
    let root = "p4-slow";

    {
        let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
        let scope = store.scope(root);
        let key = scope.key("segments/slow.bin");
        let res = scope.store().acquire_resource(&key, None).unwrap();
        res.write_at(0, b"xyz").unwrap();
        res.commit(Some(3)).unwrap();
        drop(res);
    }

    let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
    let scope = store.scope(root);
    let key = scope.key("segments/slow.bin");

    assert_eq!(scope.store().final_len(&key), Some(3));
    assert!(scope.store().contains_range(&key, 0..3));
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn mem_checkpoint_is_noop_and_aggregate_is_ephemeral() {
    let root = "p4-mem";

    {
        let store = AssetStoreBuilder::new().ephemeral(true).build();
        let scope = store.scope(root);
        let key = scope.key("segments/mem.bin");
        let res = scope.store().acquire_resource(&key, None).unwrap();
        res.write_at(0, b"abcd").unwrap();
        res.commit(Some(4)).unwrap();
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::new().ephemeral(true).build();
    let scope = store.scope(root);
    let key = scope.key("segments/mem.bin");
    assert!(scope.store().available_ranges(&key).is_empty());
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_is_idempotent() {
    let dir = tempdir().unwrap();
    let root = "p4-idem";

    let store = AssetStoreBuilder::new().root_dir(dir.path()).build();
    let scope = store.scope(root);
    let key = scope.key("segments/idempotent.bin");
    let res = scope.store().acquire_resource(&key, None).unwrap();
    res.write_at(0, b"hello").unwrap();
    res.commit(Some(5)).unwrap();
    drop(res);

    store.checkpoint().unwrap();
    store.checkpoint().unwrap();
    store.checkpoint().unwrap();

    let store2 = AssetStoreBuilder::new().root_dir(dir.path()).build();
    let scope2 = store2.scope(root);
    assert_eq!(scope2.store().final_len(&key), Some(5));
}
