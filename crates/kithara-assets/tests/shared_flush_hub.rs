//! Integration tests for the multi-store shared [`FlushHub`].
//!
//! These pin the contract that motivates `S5`: a single `FlushHub` can
//! coordinate flushes across many `AssetStore` instances, and dropping
//! a store transparently removes its indexes from the hub registry via
//! `Weak`-based GC.

#![cfg(not(target_arch = "wasm32"))]

use kithara_assets::{AssetStoreBuilder, FlushHub, FlushPolicy, ResourceKey};
use kithara_platform::time::Duration;
use kithara_storage::ResourceExt;
use kithara_test_utils::kithara;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

const INDEXES_PER_DISK_STORE: usize = 3;

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_registers_three_indexes_per_store() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::new(), FlushPolicy::default());
    assert_eq!(hub.live_source_count(), 0, "fresh hub has no sources");

    let _store_a = AssetStoreBuilder::new()
        .root_dir(dir.path().join("a"))
        .asset_root(Some("track-a"))
        .flush_hub(hub.clone())
        .build();

    assert_eq!(
        hub.live_source_count(),
        INDEXES_PER_DISK_STORE,
        "one disk store registers Pins+Lru+Availability with the hub"
    );

    let _store_b = AssetStoreBuilder::new()
        .root_dir(dir.path().join("b"))
        .asset_root(Some("track-b"))
        .flush_hub(hub.clone())
        .build();

    assert_eq!(
        hub.live_source_count(),
        INDEXES_PER_DISK_STORE * 2,
        "second disk store contributes another three indexes"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_gcs_dropped_store_indexes() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::new(), FlushPolicy::default());

    let store_a = AssetStoreBuilder::new()
        .root_dir(dir.path().join("a"))
        .asset_root(Some("track-a"))
        .flush_hub(hub.clone())
        .build();
    let store_b = AssetStoreBuilder::new()
        .root_dir(dir.path().join("b"))
        .asset_root(Some("track-b"))
        .flush_hub(hub.clone())
        .build();
    assert_eq!(hub.live_source_count(), INDEXES_PER_DISK_STORE * 2);

    let _res_a = store_a
        .acquire_resource(&ResourceKey::new("seg.bin"))
        .unwrap();
    let _res_b = store_b
        .acquire_resource(&ResourceKey::new("seg.bin"))
        .unwrap();

    drop(_res_a);
    drop(store_a);

    assert_eq!(
        hub.live_source_count(),
        INDEXES_PER_DISK_STORE,
        "dropping store A must GC its three indexes from the hub"
    );

    drop(_res_b);
    hub.flush_now()
        .expect("flush_now must succeed for the surviving store");

    drop(store_b);
    assert_eq!(
        hub.live_source_count(),
        0,
        "all stores destroyed → registry empties"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_flush_now_persists_every_store() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::new(), FlushPolicy::default());

    let store_a = AssetStoreBuilder::new()
        .root_dir(dir.path().join("a"))
        .asset_root(Some("track-a"))
        .flush_hub(hub.clone())
        .build();
    let store_b = AssetStoreBuilder::new()
        .root_dir(dir.path().join("b"))
        .asset_root(Some("track-b"))
        .flush_hub(hub.clone())
        .build();

    let key = ResourceKey::new("payload.bin");
    let res_a = store_a.acquire_resource(&key).unwrap();
    res_a.write_at(0, b"alpha").unwrap();
    res_a.commit(Some(5)).unwrap();
    let res_b = store_b.acquire_resource(&key).unwrap();
    res_b.write_at(0, b"bravo").unwrap();
    res_b.commit(Some(5)).unwrap();
    drop(res_a);
    drop(res_b);

    let avail_a = dir.path().join("a/_index/availability.bin");
    let avail_b = dir.path().join("b/_index/availability.bin");
    assert!(!avail_a.exists(), "availability is checkpoint-only");
    assert!(!avail_b.exists(), "availability is checkpoint-only");

    hub.flush_now().expect("hub.flush_now must succeed");

    assert!(
        avail_a.metadata().is_ok_and(|m| m.len() > 0),
        "store A availability must land after shared flush_now"
    );
    assert!(
        avail_b.metadata().is_ok_and(|m| m.len() > 0),
        "store B availability must land after shared flush_now"
    );
}
