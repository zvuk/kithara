#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! Integration test: three on-disk index files live alongside the
//! asset data and reflect runtime state.
//!
//! Covers the question "when do `pins.bin` / `lru.bin` /
//! `availability.bin` actually hit the disk?" Spawns a realistic
//! acquire → write → commit → read sequence and asserts both the
//! existence and the content of each file at every intermediate
//! step, because each of the three indexes has a different
//! persistence contract:
//!
//! | File              | Persisted by                       | Eagerly flushed?     |
//! | `pins.bin`        | `LeaseAssets::pin` / `Drop` on      | yes, every pin/unpin |
//! |                   | `LeaseGuard`                        |                      |
//! | `lru.bin`         | `EvictAssets::touch_and_maybe_evict`| yes, every acquire   |
//! | `availability.bin`| `AssetStore::checkpoint()`          | **NO** — explicit    |
//!
//! If any of the above contracts regresses, this test flips first.
//! It also validates the on-disk rkyv payload by reconstructing
//! the archived view and checking the version / pins set /
//! availability ranges that we just wrote.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use kithara_assets::{
    AssetStore, AssetStoreBuilder, EvictConfig, ResourceKey,
    index::schema::{ArchivedAvailabilityFile, ArchivedLruIndexFile, ArchivedPinsIndexFile},
};
use kithara_storage::ResourceExt;
use kithara_test_utils::{kithara, temp_dir};
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

fn read_archived_availability_ranges(path: &Path, asset_root: &str, key: &str) -> Vec<(u64, u64)> {
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
    res.ranges
        .iter()
        .map(|r| (r.0.to_native(), r.1.to_native()))
        .collect()
}

fn read_archived_availability_final_len(path: &Path, asset_root: &str, key: &str) -> Option<u64> {
    let bytes = fs::read(path).expect("read availability.bin");
    let archived = rkyv::access::<ArchivedAvailabilityFile, rkyv::rancor::Error>(&bytes)
        .expect("availability.bin must be a valid rkyv payload");
    let asset = archived.assets.get(asset_root).expect("asset entry");
    let res = asset.resources.get(key).expect("resource entry");
    match res.final_len {
        ArchivedOption::Some(ref l) => Some(l.to_native()),
        ArchivedOption::None => None,
    }
}

fn build_store(temp_dir: &kithara_test_utils::TestTempDir, asset_root: &str) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(Some(asset_root))
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build()
}

/// End-to-end persistence contract: realistic acquire → write →
/// commit → read lifecycle, checking every index file at every
/// observable step.
#[kithara::test(timeout(Duration::from_secs(5)))]
fn index_files_persisted_during_real_workload(temp_dir: kithara_test_utils::TestTempDir) {
    let root = temp_dir.path().to_path_buf();
    let asset_root = "persisted-asset";
    let pins = pins_path(&root);
    let lru = lru_path(&root);
    let availability = availability_path(&root);

    // 1) Fresh store — no index files yet.
    let store = build_store(&temp_dir, asset_root);
    assert!(
        !pins.exists(),
        "pins.bin must not exist before any activity"
    );
    assert!(!lru.exists(), "lru.bin must not exist before any activity");
    assert!(
        !availability.exists(),
        "availability.bin must not exist before checkpoint()"
    );

    // 2) Acquire a resource: this eagerly flushes both pins.bin
    //    (through LeaseAssets::pin) and lru.bin (through
    //    EvictAssets::touch_and_maybe_evict).
    let key_a = ResourceKey::new("segment-a.bin");
    let res_a = store.acquire_resource(&key_a).expect("acquire segment-a");
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
        "availability.bin is checkpoint-only; must still be absent"
    );

    // Pins payload: exactly one pin for our asset_root.
    let pinned_now = read_archived_pins_set(&pins);
    assert!(
        pinned_now.contains(&asset_root.to_string()),
        "pins.bin must contain our asset_root while a resource is live; got {pinned_now:?}"
    );
    assert_eq!(read_archived_pins_version(&pins), 1);

    // LRU payload: our asset_root must be tracked.
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

    // 3) Write + commit. Availability is updated in memory only.
    let payload = b"integration-payload-0123456789abcdef";
    res_a.write_at(0, payload).expect("write segment-a");
    res_a
        .commit(Some(payload.len() as u64))
        .expect("commit segment-a");
    assert!(
        !availability.exists(),
        "commit() must NOT auto-persist availability.bin (explicit checkpoint contract)"
    );

    // Query the in-memory aggregate to prove the observer wired up.
    let ranges_in_memory = store.available_ranges(&key_a);
    let as_vec: Vec<(u64, u64)> = ranges_in_memory.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(
        as_vec,
        vec![(0, payload.len() as u64)],
        "in-memory availability must reflect the committed range"
    );

    // 4) Second resource touches LRU again: clock advances.
    let key_b = ResourceKey::new("segment-b.bin");
    let res_b = store.acquire_resource(&key_b).expect("acquire segment-b");
    let clock_after_second = read_archived_lru_clock(&lru);
    assert!(
        clock_after_second > clock_after_first,
        "lru clock must advance on subsequent acquire \
         ({clock_after_second} <= {clock_after_first})"
    );
    res_b.write_at(0, b"b-payload").expect("write b");
    res_b.commit(Some(9)).expect("commit b");

    // 5) Explicit checkpoint: now availability.bin appears.
    assert!(
        !availability.exists(),
        "sanity: still no file pre-checkpoint"
    );
    store.checkpoint().expect("checkpoint must succeed");
    assert!(
        has_nonempty(&availability),
        "availability.bin must exist and be non-empty after checkpoint()"
    );

    // Availability payload: both committed ranges must round-trip.
    let ranges_a = read_archived_availability_ranges(&availability, asset_root, "segment-a.bin");
    assert_eq!(ranges_a, vec![(0, payload.len() as u64)]);
    assert_eq!(
        read_archived_availability_final_len(&availability, asset_root, "segment-a.bin"),
        Some(payload.len() as u64),
    );
    let ranges_b = read_archived_availability_ranges(&availability, asset_root, "segment-b.bin");
    assert_eq!(ranges_b, vec![(0, 9)]);

    // 6) Release resources: pins.bin must shrink back to empty.
    drop(res_a);
    drop(res_b);
    // Let any drop-side persistence finish.
    let pinned_after_drop = read_archived_pins_set(&pins);
    assert!(
        pinned_after_drop.is_empty(),
        "after dropping the last live resource, no asset_root should remain pinned; got {pinned_after_drop:?}"
    );

    // 7) Open a third time and make sure availability.bin still
    //    reflects what we checkpointed — i.e. checkpoint is a real
    //    snapshot, not a race-prone temporary.
    let reopened = build_store(&temp_dir, asset_root);
    let rehydrated = reopened.available_ranges(&key_a);
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
fn index_files_land_under_root_dir_index(temp_dir: kithara_test_utils::TestTempDir) {
    let root = temp_dir.path().to_path_buf();
    let store = build_store(&temp_dir, "basic-asset");
    let key = ResourceKey::new("one.bin");
    {
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"x").unwrap();
        res.commit(Some(1)).unwrap();
    }
    store.checkpoint().unwrap();

    for p in [pins_path(&root), lru_path(&root), availability_path(&root)] {
        assert!(
            has_nonempty(&p),
            "expected index file at {} to be non-empty on disk",
            p.display()
        );
    }
}
