#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::Path};

use kithara_assets::{
    AcquisitionResult, AssetStoreBuilder, FlushHub, FlushPolicy, ReadSide, WriteSide,
};
use kithara_platform::{CancelToken, time::Duration};
use kithara_test_utils::kithara;
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

struct Consts;

impl Consts {
    const ASSET_ROOT: &'static str = "crash-test";
    const KEY_NAME: &'static str = "segments/0001.bin";
}

fn pins_bin(root: &Path) -> std::path::PathBuf {
    root.join("_index/pins.bin")
}

fn lru_bin(root: &Path) -> std::path::PathBuf {
    root.join("_index/lru.bin")
}

fn availability_bin(root: &Path) -> std::path::PathBuf {
    root.join("_index/availability.bin")
}

fn segment_path(root: &Path) -> std::path::PathBuf {
    root.join(Consts::ASSET_ROOT).join(Consts::KEY_NAME)
}

/// Write a complete segment + flush every index file. Uses an
/// explicit `checkpoint()` so the on-disk state matches what a clean
/// shutdown would produce. The closure runs *after* checkpoint and
/// before drop — the place to inject a "crash" by mangling files.
fn seed_clean_state_then(dir: &Path, mangle: impl FnOnce(&Path)) {
    let store = AssetStoreBuilder::<()>::default().root_dir(dir).build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);
    write_commit(
        store.acquire_resource(&key, None).expect("acquire"),
        b"hello-world!",
    );
    store.checkpoint().expect("checkpoint");
    mangle(dir);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn truncated_pins_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(pins_bin(root), b"").unwrap();
    });

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();

    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);
    let _res = store
        .acquire_resource(&key, None)
        .expect("rebuild over zero-byte pins.bin must still acquire");
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn garbage_pins_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(pins_bin(root), b"NOT-RKYV-PAYLOAD-AT-ALL").unwrap();
    });

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);
    let _res = store
        .acquire_resource(&key, None)
        .expect("garbage pins.bin must not block rebuild");
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn garbage_lru_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(lru_bin(root), [0xff; 64]).unwrap();
    });

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();

    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);
    assert_eq!(scope.store().final_len(&key), Some(12));
    assert!(scope.store().contains_range(&key, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn garbage_availability_bin_is_treated_as_empty() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(availability_bin(root), b"corrupted-bytes-here").unwrap();
    });

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();

    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);
    assert_eq!(
        scope.store().final_len(&key),
        Some(12),
        "slow-path must recover committed segments when availability.bin is unreadable"
    );
    assert!(scope.store().contains_range(&key, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn segment_deleted_externally_after_checkpoint_degrades_gracefully() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::remove_file(segment_path(root)).unwrap();
    });

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);

    assert_eq!(
        scope.store().final_len(&key),
        Some(12),
        "aggregate is not re-verified against disk on hydration"
    );

    match scope.store().acquire_resource(&key, None) {
        Ok(acq) => {
            let mut buf = Vec::new();
            let _ = match acq {
                AcquisitionResult::Pending(w) => w.reader().read_into(&mut buf),
                AcquisitionResult::Ready(r) => r.read_into(&mut buf),
                _ => Ok(0),
            };
        }
        Err(e) => {
            tracing::debug!(error = %e, "stale-claim resource correctly errored on acquire");
        }
    }
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn partial_segment_with_no_commit_and_no_checkpoint_is_invisible_after_crash() {
    let dir = tempdir().unwrap();

    {
        let store = AssetStoreBuilder::<()>::default()
            .root_dir(dir.path())
            .build();
        let scope = store.scope(Consts::ASSET_ROOT);
        let res = pending(
            store
                .acquire_resource(&scope.key(Consts::KEY_NAME), None)
                .unwrap(),
        );
        res.write_at(0, b"partial-bytes").unwrap();
        drop(res);
    }

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);

    assert!(scope.store().available_ranges(&key).is_empty());
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn partial_uncommitted_write_flushed_before_drop_is_invisible_after_crash() {
    let dir = tempdir().unwrap();

    {
        let store = AssetStoreBuilder::<()>::default()
            .root_dir(dir.path())
            .build();
        let scope = store.scope(Consts::ASSET_ROOT);
        let res = pending(
            store
                .acquire_resource(&scope.key(Consts::KEY_NAME), None)
                .unwrap(),
        );
        res.write_at(0, b"partial-bytes").unwrap();
        // Force the availability snapshot to disk WHILE the uncommitted writer
        // is still alive. This deterministically reproduces the worker-wins race
        // (background flush beating the writer's cleanup): the persisted file
        // must still omit the uncommitted partial range, exactly as the slow
        // path reports the never-renamed `.tmp` Missing.
        store.checkpoint().expect("checkpoint");
        drop(res);
    }

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);

    assert!(scope.store().available_ranges(&key).is_empty());
    assert_eq!(scope.store().final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn commit_then_crash_before_checkpoint_recovers_via_slow_path() {
    let dir = tempdir().unwrap();

    {
        let store = AssetStoreBuilder::<()>::default()
            .root_dir(dir.path())
            .build();
        let scope = store.scope(Consts::ASSET_ROOT);
        write_commit(
            store
                .acquire_resource(&scope.key(Consts::KEY_NAME), None)
                .unwrap(),
            b"durable-data",
        );
    }

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);

    assert_eq!(scope.store().final_len(&key), Some(12));
    assert!(scope.store().contains_range(&key, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn crash_between_per_store_flushes_keeps_each_store_independently_consistent() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());

    let dir_a = dir.path().join("a");
    let dir_b = dir.path().join("b");

    {
        let store_a = AssetStoreBuilder::<()>::default()
            .root_dir(&dir_a)
            .flush_hub(hub.clone())
            .build();
        let store_b = AssetStoreBuilder::<()>::default()
            .root_dir(&dir_b)
            .flush_hub(hub.clone())
            .build();

        let scope_a = store_a.scope("track-a");
        write_commit(
            store_a
                .acquire_resource(&scope_a.key(Consts::KEY_NAME), None)
                .unwrap(),
            b"alpha-data!!",
        );

        let scope_b = store_b.scope("track-b");
        write_commit(
            store_b
                .acquire_resource(&scope_b.key(Consts::KEY_NAME), None)
                .unwrap(),
            b"bravo-data!!",
        );

        store_a.checkpoint().unwrap();
    }

    let rebuilt_a = AssetStoreBuilder::<()>::default().root_dir(&dir_a).build();
    let rebuilt_b = AssetStoreBuilder::<()>::default().root_dir(&dir_b).build();
    let scope_a = rebuilt_a.scope("track-a");
    let scope_b = rebuilt_b.scope("track-b");
    let key_a = scope_a.key(Consts::KEY_NAME);
    let key_b = scope_b.key(Consts::KEY_NAME);

    assert_eq!(rebuilt_a.final_len(&key_a), Some(12));
    assert!(rebuilt_a.contains_range(&key_a, 0..12));
    assert_eq!(
        rebuilt_b.final_len(&key_b),
        Some(12),
        "sibling without checkpoint still recovers via slow-path"
    );
    assert!(rebuilt_b.contains_range(&key_b, 0..12));
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn red_segment_file_must_not_be_visible_at_canonical_path_before_commit() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let res = pending(
        store
            .acquire_resource(&scope.key(Consts::KEY_NAME), None)
            .unwrap(),
    );
    res.write_at(0, b"partial-bytes").unwrap();

    let canonical = segment_path(dir.path());
    let canonical_visible_with_bytes = canonical.metadata().is_ok_and(|m| m.len() > 0);
    assert!(
        !canonical_visible_with_bytes,
        "segment must not be observable at its canonical path before commit; \
         current state leaks partial bytes to any external reader"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn red_kill9_mid_write_must_not_leave_canonical_file_with_partial_bytes() {
    let dir = tempdir().unwrap();
    {
        let store = AssetStoreBuilder::<()>::default()
            .root_dir(dir.path())
            .build();
        let scope = store.scope(Consts::ASSET_ROOT);
        let res = pending(
            store
                .acquire_resource(&scope.key(Consts::KEY_NAME), None)
                .unwrap(),
        );
        res.write_at(0, b"partial-bytes-from-killed-writer")
            .unwrap();
        std::mem::forget(res);
        std::mem::forget(store);
    }

    let canonical = segment_path(dir.path());
    if canonical.exists() {
        let len = canonical.metadata().unwrap().len();
        assert_eq!(
            len, 0,
            "kill -9 mid-write must not leave a non-empty canonical segment file; \
             found {len} bytes at {canonical:?}"
        );
    }

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);
    assert_eq!(
        store.final_len(&key),
        None,
        "after kill -9 mid-write, no resource state must claim the partial file"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn red_canonical_path_must_have_exact_bytes_after_commit_no_initial_mmap_padding() {
    let dir = tempdir().unwrap();
    let payload = b"exactly-12-b";
    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let res = pending(
        store
            .acquire_resource(&scope.key(Consts::KEY_NAME), None)
            .unwrap(),
    );
    res.write_at(0, payload).unwrap();

    let canonical = segment_path(dir.path());
    let mid_write_size = canonical.metadata().map_or(0, |m| m.len());
    assert_eq!(
        mid_write_size, 0,
        "mid-write the canonical path must contain zero observable bytes (got {mid_write_size})"
    );

    drop(res.commit(Some(payload.len() as u64)).unwrap());

    let on_disk = fs::read(&canonical).expect("canonical exists post-commit");
    assert_eq!(
        on_disk.len(),
        payload.len(),
        "post-commit canonical file must be exactly final_len bytes (no mmap padding)"
    );
    assert_eq!(on_disk.as_slice(), payload);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn doubly_corrupted_indexes_do_not_panic_and_slow_path_serves_data() {
    let dir = tempdir().unwrap();
    seed_clean_state_then(dir.path(), |root| {
        fs::write(pins_bin(root), b"").unwrap();
        fs::write(lru_bin(root), b"\xfe\xfe\xfe").unwrap();
        fs::write(availability_bin(root), b"PARTIAL!").unwrap();
    });

    let store = AssetStoreBuilder::<()>::default()
        .root_dir(dir.path())
        .build();
    let scope = store.scope(Consts::ASSET_ROOT);
    let key = scope.key(Consts::KEY_NAME);

    assert_eq!(
        scope.store().final_len(&key),
        Some(12),
        "every index corrupt → slow-path still works"
    );
    assert!(scope.store().contains_range(&key, 0..12));

    store
        .checkpoint()
        .expect("checkpoint over cleaned state must succeed");
}
