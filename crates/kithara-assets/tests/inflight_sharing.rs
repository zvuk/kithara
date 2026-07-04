//! Behavioural tests for in-flight asset sharing identity contract.
//!
//! These tests pin the post-refactor contract:
//! - `asset_root` is a method parameter, not store-level state.
//! - `RequestIdentity` differentiates inflight handles within one store.
//! - Distinct `AssetStore` instances stay isolated by construction.
//!
//! See `.docs/plans/2026-05-20-inflight-asset-sharing.md` step 1.

use kithara_assets::{
    AcquisitionResult, AssetStoreBuilder, ReadSide, RequestIdentity, StorageBackend, WriteSide,
};
use kithara_platform::time::Duration;
use kithara_storage::ResourceStatus;
use kithara_test_utils::kithara;
use tempfile::tempdir;

/// Extract the Pending writer or panic.
fn pending<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>) -> W {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn one_store_same_url_same_identity_shares_inner() {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope = store.scope("asset_a");
    let key = scope.key("audio.mp3");
    let id = RequestIdentity::from_headers([("authorization", b"Bearer x".as_slice())]);

    let w = pending(scope.store().acquire_resource(&key, Some(&id)).unwrap());
    w.write_at(0, b"hello").unwrap();

    // A read view opened for the same (url, identity) must observe the write
    // through the shared inner storage.
    let r2 = scope.store().open_resource(&key, Some(&id)).unwrap();
    let mut buf = [0u8; 5];
    let n = r2.read_at(0, &mut buf).unwrap();
    assert_eq!(
        n, 5,
        "second handle must observe writes through shared inner"
    );
    assert_eq!(&buf, b"hello");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn one_store_same_url_different_identity_yields_different_inner() {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope = store.scope("asset_a");
    let key = scope.key("audio.mp3");
    let id1 = RequestIdentity::from_headers([("authorization", b"Bearer a".as_slice())]);
    let id2 = RequestIdentity::from_headers([("authorization", b"Bearer b".as_slice())]);

    let w = pending(scope.store().acquire_resource(&key, Some(&id1)).unwrap());
    w.write_at(0, b"hello").unwrap();

    // A distinct identity has no shared inner: opening it must not surface the
    // first identity's write (missing resource, or an empty read).
    match scope.store().open_resource(&key, Some(&id2)) {
        Err(_) => {}
        Ok(r2) => {
            let mut buf = [0u8; 5];
            assert!(
                r2.read_at(0, &mut buf).map_or(true, |n| n == 0),
                "second identity must not observe writes from first identity"
            );
        }
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn one_store_two_asset_roots_isolated() {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope_a = store.scope("root_a");
    let scope_b = store.scope("root_b");
    let id = RequestIdentity::empty();

    let w_a = pending(
        store
            .acquire_resource(&scope_a.key("audio.mp3"), Some(&id))
            .unwrap(),
    );
    w_a.write_at(0, b"hello").unwrap();

    match store.open_resource(&scope_b.key("audio.mp3"), Some(&id)) {
        Err(_) => {}
        Ok(r_b) => {
            let mut buf = [0u8; 5];
            assert!(
                r_b.read_at(0, &mut buf).map_or(true, |n| n == 0),
                "different asset_root within one store must remain isolated"
            );
        }
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn two_stores_isolated_even_with_same_identity() {
    let store_a = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let store_b = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope_a = store_a.scope("root");
    let scope_b = store_b.scope("root");
    let id = RequestIdentity::empty();

    let w_a = pending(
        store_a
            .acquire_resource(&scope_a.key("audio.mp3"), Some(&id))
            .unwrap(),
    );
    w_a.write_at(0, b"hello").unwrap();

    match store_b.open_resource(&scope_b.key("audio.mp3"), Some(&id)) {
        Err(_) => {}
        Ok(r_b) => {
            let mut buf = [0u8; 5];
            assert!(
                r_b.read_at(0, &mut buf).map_or(true, |n| n == 0),
                "distinct AssetStore instances must be fully isolated"
            );
        }
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn drop_first_leaves_second_alive() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope("root");
    let key = scope.key("audio.mp3");
    let id = RequestIdentity::empty();

    let w1 = pending(scope.store().acquire_resource(&key, Some(&id)).unwrap());
    let r2 = w1.reader();

    w1.write_at(0, b"hello").unwrap();
    let committed = w1.commit(Some(5)).unwrap();
    drop(committed);

    // The read view acquired before commit stays alive after the writer's
    // committed reader drops, and observes the committed bytes.
    let mut buf = [0u8; 5];
    let n = r2.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");
    drop(r2);
}

/// Test A from the shared-AssetStore plan: pins the shared-availability
/// contract. One store, one `asset_root`, two handles acquired before any
/// commit — write+commit through the first must surface as `Committed`
/// status on the second and a populated `final_len` on the store.
/// Regressing this would mean shared playback+waveform stops seeing the
/// same data.
#[kithara::test(timeout(Duration::from_secs(5)))]
fn shared_inner_propagates_commit_and_final_len() {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .build();
    let scope = store.scope("asset_a");
    let key = scope.key("audio.mp3");

    let w1 = pending(scope.store().acquire_resource(&key, None).unwrap());
    let r2 = w1.reader();

    w1.write_at(0, b"hello").unwrap();
    let _committed = w1.commit(Some(5)).unwrap();

    assert!(
        matches!(r2.status(), ResourceStatus::Committed { .. }),
        "second handle must observe Committed status after first handle commits"
    );
    assert_eq!(
        scope.store().final_len(&key),
        Some(5),
        "final_len must reflect the committed length via shared availability"
    );
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn request_identity_hash_stable() {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };

    let id1 = RequestIdentity::from_headers([
        ("Authorization", b"Bearer x".as_slice()),
        ("X-Trace", b"abc".as_slice()),
    ]);
    let id2 = RequestIdentity::from_headers([
        ("x-trace", b"abc".as_slice()),
        ("authorization", b"Bearer x".as_slice()),
    ]);

    assert_eq!(
        id1, id2,
        "identity must be order- and case-insensitive on names"
    );

    let mut h1 = DefaultHasher::new();
    let mut h2 = DefaultHasher::new();
    id1.hash(&mut h1);
    id2.hash(&mut h2);
    assert_eq!(
        h1.finish(),
        h2.finish(),
        "hash must be stable across orderings"
    );
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn request_identity_debug_leaks_no_header_values() {
    let id = RequestIdentity::from_headers([
        ("Authorization", b"Bearer supersecret".as_slice()),
        ("Cookie", b"session=abcdef".as_slice()),
        ("X-Trace", b"public".as_slice()),
    ]);

    // Debug prints only a stable hash — never header names or values.
    let dbg = format!("{:?}", id);
    assert!(
        !dbg.contains("supersecret"),
        "secret bearer must not leak: {dbg}"
    );
    assert!(!dbg.contains("abcdef"), "cookie value must not leak: {dbg}");
    assert!(
        !dbg.contains("public"),
        "no header value should appear: {dbg}"
    );
    assert!(dbg.starts_with("RequestIdentity("), "{dbg}");
}
