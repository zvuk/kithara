#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use kithara::{
    assets::{
        AcquisitionResult, AssetScope, AssetStoreBuilder, EvictConfig, WriteSide,
        index::schema::{ArchivedAvailabilityFile, ArchivedLruIndexFile, ArchivedPinsIndexFile},
    },
    platform::time::Duration,
};
use kithara_integration_tests::{kithara, temp_dir};
use rkyv::option::ArchivedOption;

fn index_dir(root: &Path) -> PathBuf {
    root.join("_index")
}
fn pins_path(root: &Path) -> PathBuf {
    index_dir(root).join("pins.bin")
}
fn lru_path(root: &Path) -> PathBuf {
    index_dir(root).join("lru.bin")
}
fn availability_path(root: &Path) -> PathBuf {
    index_dir(root).join("availability.bin")
}

fn has_nonempty(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

fn read_archived_pins_version(path: &Path) -> u32 {
    let bytes = fs::read(path).expect("read pins.bin");
    let archived = rkyv::access::<ArchivedPinsIndexFile, rkyv::rancor::Error>(&bytes)
        .expect("pins.bin must be a valid rkyv payload");
    archived.version.to_native()
}

fn read_archived_pins_set(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).expect("read pins.bin");
    let archived = rkyv::access::<ArchivedPinsIndexFile, rkyv::rancor::Error>(&bytes)
        .expect("pins.bin must be a valid rkyv payload");
    archived
        .pinned
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| k.as_str().to_string())
        .collect()
}

fn read_archived_lru_clock(path: &Path) -> u64 {
    let bytes = fs::read(path).expect("read lru.bin");
    let archived = rkyv::access::<ArchivedLruIndexFile, rkyv::rancor::Error>(&bytes)
        .expect("lru.bin must be a valid rkyv payload");
    archived.clock.to_native()
}

fn read_archived_lru_keys(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).expect("read lru.bin");
    let archived = rkyv::access::<ArchivedLruIndexFile, rkyv::rancor::Error>(&bytes)
        .expect("lru.bin must be a valid rkyv payload");
    archived
        .entries
        .iter()
        .map(|(k, _)| k.as_str().to_string())
        .collect()
}

struct ArchivedResourceEntry {
    ranges: Vec<(u64, u64)>,
    final_len: Option<u64>,
}

fn read_archived_availability(path: &Path, asset_root: &str, key: &str) -> ArchivedResourceEntry {
    let bytes = fs::read(path).expect("read availability.bin");
    let archived = rkyv::access::<ArchivedAvailabilityFile, rkyv::rancor::Error>(&bytes)
        .expect("availability.bin must be a valid rkyv payload");
    let asset = archived
        .assets
        .get(asset_root)
        .expect("asset entry must be persisted");
    let res = asset
        .resources
        .get(key)
        .expect("resource entry must be persisted");
    ArchivedResourceEntry {
        ranges: res
            .ranges
            .iter()
            .map(|r| (r.0.to_native(), r.1.to_native()))
            .collect(),
        final_len: match res.final_len {
            ArchivedOption::Some(ref l) => Some(l.to_native()),
            ArchivedOption::None => None,
        },
    }
}

fn build_scope(temp_dir: &kithara_integration_tests::TestTempDir, asset_root: &str) -> AssetScope {
    AssetStoreBuilder::default()
        .root_dir(temp_dir.path())
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build()
        .scope(asset_root)
}

/// End-to-end persistence contract: realistic acquire → write →
/// commit → read lifecycle, checking every index file at every
/// observable step.
#[kithara::test(timeout(Duration::from_secs(5)))]
fn index_files_persisted_during_real_workload(temp_dir: kithara_integration_tests::TestTempDir) {
    let root = temp_dir.path().to_path_buf();
    let asset_root = "persisted-asset";
    let pins = pins_path(&root);
    let lru = lru_path(&root);
    let availability = availability_path(&root);

    let scope = build_scope(&temp_dir, asset_root);
    assert!(
        !pins.exists(),
        "pins.bin must not exist before any activity"
    );
    assert!(!lru.exists(), "lru.bin must not exist before any activity");
    assert!(
        !availability.exists(),
        "no commit yet → the flush worker has nothing to persist"
    );

    let key_a = scope.key("segment-a.bin");
    let AcquisitionResult::Pending(writer_a) = scope
        .store()
        .acquire_resource(&key_a, None)
        .expect("acquire segment-a")
    else {
        panic!("fresh acquire of segment-a must be Pending");
    };
    assert!(
        has_nonempty(&pins),
        "pins.bin must be flushed eagerly after acquire_resource"
    );
    assert!(
        has_nonempty(&lru),
        "lru.bin must be flushed eagerly after acquire_resource"
    );
    assert!(
        !availability.exists(),
        "acquire writes no bytes → still nothing for the worker to persist"
    );

    let pinned_now = read_archived_pins_set(&pins);
    assert!(
        pinned_now.contains(&asset_root.to_string()),
        "pins.bin must contain our asset_root while a resource is live; got {pinned_now:?}"
    );
    assert_eq!(read_archived_pins_version(&pins), 1);

    let lru_keys_now = read_archived_lru_keys(&lru);
    assert!(
        lru_keys_now.contains(&asset_root.to_string()),
        "lru.bin must track our asset_root after acquire; got {lru_keys_now:?}"
    );
    let clock_after_first = read_archived_lru_clock(&lru);
    assert!(
        clock_after_first >= 1,
        "lru clock must advance on first acquire (got {clock_after_first})"
    );

    let payload = b"integration-payload-0123456789abcdef";
    writer_a.write_at(0, payload).expect("write segment-a");
    let res_a = writer_a
        .commit(Some(payload.len() as u64))
        .expect("commit segment-a");
    // After a commit, availability.bin MAY be background-persisted (non-durable,
    // best-effort) by the flush worker — its on-disk presence here is
    // timing-dependent (see kithara-assets README "Byte Availability"). The
    // durable, authoritative snapshot is asserted after checkpoint() below;
    // the in-memory aggregate is the deterministic post-commit check.
    let ranges_in_memory = scope.store().available_ranges(&key_a);
    let as_vec: Vec<(u64, u64)> = ranges_in_memory.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(
        as_vec,
        vec![(0, payload.len() as u64)],
        "in-memory availability must reflect the committed range"
    );

    let key_b = scope.key("segment-b.bin");
    let AcquisitionResult::Pending(writer_b) = scope
        .store()
        .acquire_resource(&key_b, None)
        .expect("acquire segment-b")
    else {
        panic!("fresh acquire of segment-b must be Pending");
    };
    let clock_after_second = read_archived_lru_clock(&lru);
    assert_eq!(
        clock_after_second, clock_after_first,
        "lru clock must NOT re-advance for an already-tracked asset_root \
         ({clock_after_second} != {clock_after_first})"
    );
    writer_b.write_at(0, b"b-payload").expect("write b");
    let res_b = writer_b.commit(Some(9)).expect("commit b");

    scope.store().checkpoint().expect("checkpoint must succeed");
    assert!(
        has_nonempty(&availability),
        "availability.bin must exist and be non-empty after checkpoint()"
    );

    let entry_a = read_archived_availability(&availability, asset_root, "segment-a.bin");
    assert_eq!(entry_a.ranges, vec![(0, payload.len() as u64)]);
    assert_eq!(entry_a.final_len, Some(payload.len() as u64));
    let entry_b = read_archived_availability(&availability, asset_root, "segment-b.bin");
    assert_eq!(entry_b.ranges, vec![(0, 9)]);

    drop(res_a);
    drop(res_b);
    let pinned_after_drop = read_archived_pins_set(&pins);
    assert!(
        pinned_after_drop.is_empty(),
        "after dropping the last live resource, no asset_root should remain pinned; got {pinned_after_drop:?}"
    );

    let reopened = build_scope(&temp_dir, asset_root);
    let rehydrated = reopened.store().available_ranges(&key_a);
    let rehydrated_vec: Vec<(u64, u64)> = rehydrated.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(
        rehydrated_vec,
        vec![(0, payload.len() as u64)],
        "reopening the store must seed availability from availability.bin on disk"
    );
}

/// Minimal contract test: no reliance on `EvictConfig`/`LeaseAssets`
/// fine-print — just asserts the three files appear in the spots the
/// documented API promises they will.
#[kithara::test(timeout(Duration::from_secs(3)))]
fn index_files_land_under_root_dir_index(temp_dir: kithara_integration_tests::TestTempDir) {
    let root = temp_dir.path().to_path_buf();
    let scope = build_scope(&temp_dir, "basic-asset");
    let key = scope.key("one.bin");
    {
        let AcquisitionResult::Pending(writer) =
            scope.store().acquire_resource(&key, None).unwrap()
        else {
            panic!("fresh acquire must be Pending");
        };
        writer.write_at(0, b"x").unwrap();
        writer.commit(Some(1)).unwrap();
    }
    scope.store().checkpoint().unwrap();

    for p in [pins_path(&root), lru_path(&root), availability_path(&root)] {
        assert!(
            has_nonempty(&p),
            "expected index file at {} to be non-empty on disk",
            p.display()
        );
    }
}
