//! Phase P-4 integration tests for
//! [`AssetStore::checkpoint`]: verify that an explicit checkpoint
//! persists the in-memory aggregate so a freshly-built store over
//! the same cache directory observes the same byte-availability state
//! without ever reopening individual resources.
//!
//! These tests also pin the Mem-backend contract: checkpoint is a
//! no-op and the aggregate is lost on rebuild.

#![cfg(not(target_arch = "wasm32"))]

use kithara_assets::{AssetStoreBuilder, ResourceKey};
use kithara_platform::time::Duration;
use kithara_storage::ResourceExt;
use kithara_test_utils::kithara;
use tempfile::tempdir;

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_persists_committed_resource_across_rebuild() {
    let dir = tempdir().unwrap();
    let key = ResourceKey::new("segments/0001.bin");

    {
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("p4-persist"))
            .build();
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"hello world").unwrap();
        res.commit(Some(11)).unwrap();
        drop(res);
        store.checkpoint().unwrap();
    }

    // New store over the same cache dir — the aggregate should be
    // seeded from `_index/availability.bin` alone, without touching
    // the segment file itself.
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-persist"))
        .build();

    assert_eq!(store.final_len(&key), Some(11));
    assert!(store.contains_range(&key, 0..11));

    let ranges = store.available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(pairs, vec![(0, 11)]);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_persists_partial_writes_across_rebuild() {
    let dir = tempdir().unwrap();
    let key = ResourceKey::new("segments/partial.bin");

    {
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("p4-partial"))
            .build();
        let res = store.acquire_resource(&key).unwrap();
        // Write two disjoint ranges, do NOT commit.
        res.write_at(0, b"aaa").unwrap();
        res.write_at(10, b"bbb").unwrap();
        drop(res);
        store.checkpoint().unwrap();
    }

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-partial"))
        .build();

    let ranges = store.available_ranges(&key);
    let pairs: Vec<_> = ranges.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(pairs, vec![(0, 3), (10, 13)]);
    assert!(store.contains_range(&key, 0..3));
    assert!(store.contains_range(&key, 10..13));
    assert!(!store.contains_range(&key, 3..10));
    // Uncommitted → final_len unknown.
    assert_eq!(store.final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_without_prior_writes_is_noop() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-empty"))
        .build();

    // No resources touched — checkpoint must still succeed and
    // produce a rebuildable file.
    store.checkpoint().unwrap();

    let store2 = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-empty"))
        .build();

    let key = ResourceKey::new("ghost.bin");
    assert!(store2.available_ranges(&key).is_empty());
    assert_eq!(store2.final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_rebuild_without_checkpoint_falls_back_to_slow_path() {
    // If the user never calls `checkpoint()`, the aggregate is empty
    // on rebuild but the slow-path `resource_state` fallback still
    // exposes committed files on disk.
    let dir = tempdir().unwrap();
    let key = ResourceKey::new("segments/slow.bin");

    {
        let store = AssetStoreBuilder::new()
            .root_dir(dir.path())
            .asset_root(Some("p4-slow"))
            .build();
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"xyz").unwrap();
        res.commit(Some(3)).unwrap();
        drop(res);
        // NO checkpoint — drop store without persisting aggregate.
    }

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-slow"))
        .build();

    // Slow path still works: resource_state sees the committed file.
    assert_eq!(store.final_len(&key), Some(3));
    assert!(store.contains_range(&key, 0..3));
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn mem_checkpoint_is_noop_and_aggregate_is_ephemeral() {
    let key = ResourceKey::new("segments/mem.bin");

    {
        let store = AssetStoreBuilder::new()
            .asset_root(Some("p4-mem"))
            .ephemeral(true)
            .build();
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"abcd").unwrap();
        res.commit(Some(4)).unwrap();
        // Checkpoint on a mem store must be a successful no-op.
        store.checkpoint().unwrap();
    }

    // A fresh mem store sees nothing — no persistence.
    let store = AssetStoreBuilder::new()
        .asset_root(Some("p4-mem"))
        .ephemeral(true)
        .build();
    assert!(store.available_ranges(&key).is_empty());
    assert_eq!(store.final_len(&key), None);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn disk_checkpoint_is_idempotent() {
    let dir = tempdir().unwrap();
    let key = ResourceKey::new("segments/idempotent.bin");

    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-idem"))
        .build();
    let res = store.acquire_resource(&key).unwrap();
    res.write_at(0, b"hello").unwrap();
    res.commit(Some(5)).unwrap();
    drop(res);

    // Multiple checkpoints in a row must all succeed and leave the
    // persisted file in a consistent state.
    store.checkpoint().unwrap();
    store.checkpoint().unwrap();
    store.checkpoint().unwrap();

    let store2 = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .asset_root(Some("p4-idem"))
        .build();
    assert_eq!(store2.final_len(&key), Some(5));
}
