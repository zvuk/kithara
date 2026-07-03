//! Phase P-2 smoke tests for `AssetStore::{contains_range,
//! available_ranges, final_len}`.

use kithara_assets::{AcquisitionResult, AssetStoreBuilder, StorageBackend, WriteSide};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use tempfile::tempdir;

const ROOT: &str = "availability-p2";

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_store_empty_aggregate_returns_empty() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope(ROOT);

    let key = scope.key("segments/0001.bin");
    assert!(scope.store().available_ranges(&key).is_empty());
    assert!(!scope.store().contains_range(&key, 0..1));
    assert!(
        scope.store().contains_range(&key, 0..0),
        "empty range is vacuous"
    );
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn mem_store_empty_aggregate_returns_empty() {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope = store.scope(ROOT);

    let key = scope.key("segments/0001.bin");
    assert!(scope.store().available_ranges(&key).is_empty());
    assert!(!scope.store().contains_range(&key, 0..100));
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_store_slow_path_finds_committed_file() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope(ROOT);

    let key = scope.key("segments/0001.bin");
    let AcquisitionResult::Pending(res) = scope.store().acquire_resource(&key, None).unwrap()
    else {
        panic!("fresh acquire must be Pending");
    };
    res.write_at(0, b"hello world").unwrap();
    drop(res.commit(Some(11)).unwrap());

    assert_eq!(scope.store().final_len(&key), Some(11));
    assert!(scope.store().contains_range(&key, 0..11));
    assert!(scope.store().contains_range(&key, 0..5));
    assert!(!scope.store().contains_range(&key, 0..12));

    let ranges = scope.store().available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(pairs, vec![(0, 11)]);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_store_missing_resource_returns_empty() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope(ROOT);

    let key = scope.key("segments/ghost.bin");
    assert!(scope.store().available_ranges(&key).is_empty());
    assert!(!scope.store().contains_range(&key, 0..1));
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn remove_resource_clears_aggregate_remove_call() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope(ROOT);

    let key = scope.key("segments/0001.bin");
    let AcquisitionResult::Pending(res) = scope.store().acquire_resource(&key, None).unwrap()
    else {
        panic!("fresh acquire must be Pending");
    };
    res.write_at(0, b"data").unwrap();
    drop(res.commit(Some(4)).unwrap());

    scope.store().remove_resource(&key);
    assert_eq!(scope.store().final_len(&key), None);
    assert!(!scope.store().contains_range(&key, 0..4));
}
