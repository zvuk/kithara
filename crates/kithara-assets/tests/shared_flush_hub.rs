#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroUsize;

use kithara_assets::{AcquisitionResult, AssetStoreBuilder, FlushHub, FlushPolicy, WriteSide};
use kithara_platform::{CancellationToken, time::Duration};
use kithara_test_utils::kithara;
use tempfile::tempdir;

const INDEXES_PER_DISK_STORE: usize = 3;

/// Flush policy whose background worker never fires within a test's
/// lifetime, so `flush_now()` is the only writer. The availability index
/// is debounced through the worker (it rewrites on every commit), so with
/// `FlushPolicy::default()` the worker writes `availability.bin` ~50ms
/// after a commit on its own. A test that asserts the checkpoint-only
/// "nothing on disk before `flush_now`" invariant then races that 50ms
/// timer and flakes under load. A manual-only policy removes the race.
fn manual_flush_policy() -> FlushPolicy {
    FlushPolicy {
        debounce: Duration::from_secs(3600),
        poll_interval: Duration::from_secs(3600),
        force_every_n_ops: NonZeroUsize::new(usize::MAX).expect("usize::MAX is non-zero"),
    }
}

/// Stream `data` through a Pending writer and commit it.
fn write_commit<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>, data: &[u8]) {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w.write_at(0, data).expect("scripted write succeeds");
    drop(
        w.commit(Some(data.len() as u64))
            .expect("scripted commit succeeds"),
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_registers_three_indexes_per_store() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancellationToken::default(), FlushPolicy::default());
    assert_eq!(hub.live_source_count(), 0, "fresh hub has no sources");

    let _store_a = AssetStoreBuilder::new()
        .root_dir(dir.path().join("a"))
        .flush_hub(hub.clone())
        .build();

    assert_eq!(
        hub.live_source_count(),
        INDEXES_PER_DISK_STORE,
        "one disk store registers Pins+Lru+Availability with the hub"
    );

    let _store_b = AssetStoreBuilder::new()
        .root_dir(dir.path().join("b"))
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
    let hub = FlushHub::new(CancellationToken::default(), FlushPolicy::default());

    let store_a = AssetStoreBuilder::new()
        .root_dir(dir.path().join("a"))
        .flush_hub(hub.clone())
        .build();
    let store_b = AssetStoreBuilder::new()
        .root_dir(dir.path().join("b"))
        .flush_hub(hub.clone())
        .build();
    assert_eq!(hub.live_source_count(), INDEXES_PER_DISK_STORE * 2);

    let scope_a = store_a.scope("track-a");
    let _res_a = store_a
        .acquire_resource(&scope_a.key("seg.bin"), None)
        .unwrap();
    let scope_b = store_b.scope("track-b");
    let _res_b = store_b
        .acquire_resource(&scope_b.key("seg.bin"), None)
        .unwrap();

    drop(_res_a);
    drop(scope_a);
    drop(store_a);

    assert_eq!(
        hub.live_source_count(),
        INDEXES_PER_DISK_STORE,
        "dropping store A must GC its three indexes from the hub"
    );

    drop(_res_b);
    hub.flush_now()
        .expect("flush_now must succeed for the surviving store");

    drop(scope_b);
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
    // Manual-only: the background worker must not write availability before
    // the explicit flush_now, or the checkpoint-only pre-assertion races it.
    let hub = FlushHub::new(CancellationToken::default(), manual_flush_policy());

    let store_a = AssetStoreBuilder::new()
        .root_dir(dir.path().join("a"))
        .flush_hub(hub.clone())
        .build();
    let store_b = AssetStoreBuilder::new()
        .root_dir(dir.path().join("b"))
        .flush_hub(hub.clone())
        .build();

    let scope_a = store_a.scope("track-a");
    let scope_b = store_b.scope("track-b");
    write_commit(
        store_a
            .acquire_resource(&scope_a.key("payload.bin"), None)
            .unwrap(),
        b"alpha",
    );
    write_commit(
        store_b
            .acquire_resource(&scope_b.key("payload.bin"), None)
            .unwrap(),
        b"bravo",
    );

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
