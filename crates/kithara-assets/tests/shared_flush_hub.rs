#![cfg(not(target_arch = "wasm32"))]

mod support;

use kithara_assets::{
    AcquisitionResult, AssetStoreBuilder, FlushHub, FlushPolicy, StorageBackend, WriteSide,
};
use kithara_platform::{CancelToken, time::Duration};
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

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn shared_hub_flush_now_persists_every_store() {
    let dir = tempdir().unwrap();
    let hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());

    let store_a = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().join("a"),
        })
        .flush_hub(hub.clone())
        .build();
    let store_b = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: dir.path().join("b"),
        })
        .flush_hub(hub.clone())
        .build();

    let scope_a = test_scope(&store_a, "track-a");
    let scope_b = test_scope(&store_b, "track-b");
    let key_a = test_key(&scope_a, "payload.bin");
    let key_b = test_key(&scope_b, "payload.bin");
    write_commit(store_a.acquire_resource(&key_a, None).unwrap(), b"alpha");
    write_commit(store_b.acquire_resource(&key_b, None).unwrap(), b"bravo");

    let avail_a = dir.path().join("a/_index/availability.bin");
    let avail_b = dir.path().join("b/_index/availability.bin");

    // The shared flush worker may have already background-persisted availability
    // (non-durable) after the commits above — its presence here is timing-
    // dependent (see kithara-assets README). flush_now() is the explicit,
    // authoritative durable write asserted below for BOTH stores.
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
