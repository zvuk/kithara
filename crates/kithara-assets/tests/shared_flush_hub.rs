#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroUsize;

use kithara_assets::{AssetStoreBuilder, FlushHub, FlushPolicy, ResourceHandle, ResourceKey};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

/// Policy whose worker stays dormant for the whole test: a one-hour
/// debounce and an unreachable op cap mean the background worker never
/// flushes on its own, so `flush_now` is provably the persistence path.
fn dormant_worker_policy() -> FlushPolicy {
    FlushPolicy {
        debounce: Duration::from_secs(3600),
        poll_interval: Duration::from_secs(3600),
        force_every_n_ops: NonZeroUsize::new(usize::MAX).expect("usize::MAX is non-zero"),
    }
}

/// Dropping a store leaves a dangling `Weak` in the hub registry; the
/// next `flush_now` must GC it (the `retain(strong_count > 0)` pass in
/// `flush_dirty`) and still persist the surviving store — observable via
/// the survivor's availability landing and `flush_now` not erroring.
#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_flush_survives_dropped_store() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::new(), dormant_worker_policy());

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

    let key = ResourceKey::new("seg.bin");
    let res_b = store_b.acquire_resource(&key).unwrap();
    res_b.write_at(0, b"bravo").unwrap();
    res_b.commit(Some(5)).unwrap();
    drop(res_b);

    // NOTE: dropping store A before any flush leaves its registry entries dangling.
    drop(store_a);

    hub.flush_now()
        .expect("flush_now must GC the dropped store and still persist survivors");

    let avail_b = dir.path().join("b/_index/availability.bin");
    assert!(
        avail_b.metadata().is_ok_and(|m| m.len() > 0),
        "surviving store B must persist after a sibling store was dropped"
    );

    // NOTE: with all stores gone the registry fully GCs and flush stays a no-op.
    drop(store_b);
    hub.flush_now()
        .expect("flush_now must succeed with no live stores left");
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_flush_now_persists_every_store() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::new(), dormant_worker_policy());

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
