//! Phase P-2 smoke tests for `AssetStore::{contains_range,
//! available_ranges, final_len}`. The aggregate byte-availability
//! index is still empty in P-2 (no observer, no seeding), so these
//! tests only cover the slow-path `resource_state` fallback and the
//! empty-aggregate behaviour. P-3 adds the observer and P-3's
//! integration tests exercise the fast path.

use kithara_assets::{AssetStoreBuilder, ResourceKey};
use kithara_platform::time::Duration;
use kithara_storage::ResourceExt;
use kithara_test_utils::kithara;
use tempfile::tempdir;

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_store_empty_aggregate_returns_empty() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("availability-p2"))
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    assert!(store.available_ranges(&key).is_empty());
    assert!(!store.contains_range(&key, 0..1));
    assert!(store.contains_range(&key, 0..0), "empty range is vacuous");
    assert_eq!(store.final_len(&key), None);
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn mem_store_empty_aggregate_returns_empty() {
    let store = AssetStoreBuilder::new()
        .asset_root(Some("availability-p2"))
        .ephemeral(true)
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    assert!(store.available_ranges(&key).is_empty());
    assert!(!store.contains_range(&key, 0..100));
    assert_eq!(store.final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_store_slow_path_finds_committed_file() {
    // Write + commit a resource, drop the handle, then query through
    // the AssetStore methods. The P-2 aggregate is never populated
    // (no observer yet), so every query below exercises the
    // `resource_state` slow-path fallback.
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("availability-p2"))
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    let res = store.acquire_resource(&key).unwrap();
    res.write_at(0, b"hello world").unwrap();
    res.commit(Some(11)).unwrap();
    drop(res);

    assert_eq!(store.final_len(&key), Some(11));
    assert!(store.contains_range(&key, 0..11));
    assert!(store.contains_range(&key, 0..5));
    assert!(!store.contains_range(&key, 0..12));

    let ranges = store.available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(pairs, vec![(0, 11)]);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_store_missing_resource_returns_empty() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("availability-p2"))
        .build();

    // Never created — Missing state.
    let key = ResourceKey::new("segments/ghost.bin");
    assert!(store.available_ranges(&key).is_empty());
    assert!(!store.contains_range(&key, 0..1));
    assert_eq!(store.final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn remove_resource_clears_aggregate_remove_call() {
    // Phase P-2 doesn't yet populate the aggregate, so
    // `remove_resource` exercises only the fact that
    // `AssetStore::remove_resource` still unwinds cleanly and the
    // `.remove()` call on the aggregate is a no-op for unknown keys.
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("availability-p2"))
        .build();

    let key = ResourceKey::new("segments/0001.bin");
    let res = store.acquire_resource(&key).unwrap();
    res.write_at(0, b"data").unwrap();
    res.commit(Some(4)).unwrap();
    drop(res);

    store.remove_resource(&key);
    assert_eq!(store.final_len(&key), None);
    assert!(!store.contains_range(&key, 0..4));
}
