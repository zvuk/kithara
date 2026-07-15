//! Phase P-4 integration tests for availability persistence.

#![cfg(not(target_arch = "wasm32"))]

mod support;

use std::{fs, num::NonZeroUsize, path::Path};

use kithara_assets::{
    AcquisitionResult, AssetStoreBuilder, FlushHub, FlushPolicy, StorageBackend, WriteSide,
};
use kithara_platform::{
    CancelToken, thread,
    time::{Duration, Instant},
};
use kithara_test_utils::kithara;
use support::{key as test_key, scope as test_scope};
use tempfile::tempdir;

/// Stream `data` through a Pending writer and commit it.
fn write_commit<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>, data: &[u8]) {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w.write_at(0, data).expect("write_at");
    drop(w.commit(Some(data.len() as u64)).expect("commit"));
}

/// Extract the Pending writer or panic.
fn pending<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>) -> W {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w
}

fn wait_for_snapshot_change(path: &Path, previous: Option<&[u8]>) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(bytes) = fs::read(path)
            && !bytes.is_empty()
            && previous.is_none_or(|old| old != bytes)
        {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "availability worker did not publish a changed snapshot at {}",
            path.display()
        );
        thread::yield_now();
    }
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_persists_committed_resource_across_rebuild() {
    let dir = tempdir().unwrap();
    let root = "p4-persist";

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: (dir.path()).into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, "segments/0001.bin");
        write_commit(
            scope.store().acquire_resource(&key, None).unwrap(),
            b"hello world",
        );
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = test_scope(&store, root);
    let key = test_key(&scope, "segments/0001.bin");

    assert_eq!(scope.store().final_len(&key), Some(11));
    assert!(scope.store().contains_range(&key, 0..11));

    let ranges = scope.store().available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(pairs, vec![(0, 11)]);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn remove_resource_persists_availability_deletion_across_rebuild() {
    let dir = tempdir().unwrap();
    let root = "remove-resource-persist";
    let key_name = "master.m3u8";

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, key_name);
        write_commit(
            scope.store().acquire_resource(&key, None).unwrap(),
            b"poisoned",
        );
        store.checkpoint().unwrap();
    }

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, key_name);
        assert!(store.contains_range(&key, 0..8));
        store.remove_resource(&key).unwrap();
        assert!(!store.contains_range(&key, 0..8));
        store.checkpoint().unwrap();
    }

    let reopened = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .build();
    let scope = test_scope(&reopened, root);
    let key = test_key(&scope, key_name);
    assert!(!reopened.contains_range(&key, 0..8));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn delete_asset_persists_availability_deletion_across_rebuild() {
    let dir = tempdir().unwrap();
    let root = "delete-asset-persist";
    let key_name = "master.m3u8";

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, key_name);
        write_commit(
            scope.store().acquire_resource(&key, None).unwrap(),
            b"poisoned",
        );
        store.checkpoint().unwrap();
    }

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: dir.path().into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, key_name);
        assert!(store.contains_range(&key, 0..8));
        scope.delete_asset().unwrap();
        assert!(!store.contains_range(&key, 0..8));
        store.checkpoint().unwrap();
    }

    let reopened = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .build();
    let scope = test_scope(&reopened, root);
    let key = test_key(&scope, key_name);
    assert!(!reopened.contains_range(&key, 0..8));
}

#[kithara::test(native, flash(false), timeout(Duration::from_secs(5)))]
fn worker_persists_resource_deletion_without_checkpoint() {
    let dir = tempdir().unwrap();
    let root = "worker-delete-persist";
    let key_name = "empty.m3u8";
    let hub = FlushHub::new(
        CancelToken::never(),
        FlushPolicy {
            debounce: Duration::ZERO,
            poll_interval: Duration::from_millis(100),
            force_every_n_ops: NonZeroUsize::MIN,
        },
    );
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .flush_hub(hub)
        .build();
    let scope = test_scope(&store, root);
    let key = test_key(&scope, key_name);
    let resource_path = dir
        .path()
        .join(scope.asset_root())
        .join(key.rel_path().expect("relative test key"));
    let writer = pending(store.acquire_resource(&key, None).unwrap());
    drop(writer.commit(Some(0)).unwrap());

    let availability_path = dir.path().join("_index/availability.bin");
    let before = wait_for_snapshot_change(&availability_path, None);
    assert!(resource_path.is_file());

    store.remove_resource(&key).unwrap();
    assert!(!resource_path.exists());
    let after = wait_for_snapshot_change(&availability_path, Some(&before));
    assert_ne!(before, after);

    let reopened = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().into(),
        })
        .build();
    assert_eq!(reopened.final_len(&key), None);
    assert!(reopened.available_ranges(&key).is_empty());
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_drops_partial_writes_when_writer_abandons_without_commit() {
    let dir = tempdir().unwrap();
    let root = "p4-partial";

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: (dir.path()).into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, "segments/partial.bin");
        let res = pending(scope.store().acquire_resource(&key, None).unwrap());
        res.write_at(0, b"aaa").unwrap();
        res.write_at(10, b"bbb").unwrap();
        drop(res);
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = test_scope(&store, root);
    let key = test_key(&scope, "segments/partial.bin");

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
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();

    store.checkpoint().unwrap();

    let store2 = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope2 = test_scope(&store2, root);
    let key = test_key(&scope2, "ghost.bin");

    assert!(scope2.store().available_ranges(&key).is_empty());
    assert_eq!(scope2.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_rebuild_without_checkpoint_falls_back_to_slow_path() {
    let dir = tempdir().unwrap();
    let root = "p4-slow";

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: (dir.path()).into(),
            })
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, "segments/slow.bin");
        write_commit(scope.store().acquire_resource(&key, None).unwrap(), b"xyz");
    }

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = test_scope(&store, root);
    let key = test_key(&scope, "segments/slow.bin");

    assert_eq!(scope.store().final_len(&key), Some(3));
    assert!(scope.store().contains_range(&key, 0..3));
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn mem_checkpoint_is_noop_and_aggregate_is_ephemeral() {
    let root = "p4-mem";

    {
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build();
        let scope = test_scope(&store, root);
        let key = test_key(&scope, "segments/mem.bin");
        write_commit(scope.store().acquire_resource(&key, None).unwrap(), b"abcd");
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope = test_scope(&store, root);
    let key = test_key(&scope, "segments/mem.bin");
    assert!(scope.store().available_ranges(&key).is_empty());
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_is_idempotent() {
    let dir = tempdir().unwrap();
    let root = "p4-idem";

    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = test_scope(&store, root);
    let key = test_key(&scope, "segments/idempotent.bin");
    write_commit(
        scope.store().acquire_resource(&key, None).unwrap(),
        b"hello",
    );

    store.checkpoint().unwrap();
    store.checkpoint().unwrap();
    store.checkpoint().unwrap();

    let store2 = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope2 = test_scope(&store2, root);
    assert_eq!(scope2.store().final_len(&key), Some(5));
}
