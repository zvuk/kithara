//! Contract for `AssetStore::subscribe_eviction`: a per-`asset_root`
//! eviction fanout, modelled on the demand subscription. A subscriber
//! receives every `ResourceKey` evicted under its `asset_root`; keys
//! under a different `asset_root` are not delivered to it; dropping the
//! returned guard deregisters, so no further keys arrive.
use std::{num::NonZeroUsize, sync::Arc};

use kithara_assets::{AcquisitionResult, AssetStore, AssetStoreBuilder, ResourceKey, WriteSide};
use kithara_platform::{time::Duration, tokio::sync::mpsc};
use kithara_test_utils::kithara;

const ROOT_A: &str = "asset_root_a";
const ROOT_B: &str = "asset_root_b";

fn ephemeral_store(cap: usize) -> AssetStore {
    AssetStoreBuilder::default()
        .ephemeral(true)
        .cache_capacity(NonZeroUsize::new(cap).expect("test cache capacity must be non-zero"))
        .build()
}

/// Stream `data` through a Pending writer and commit it.
fn write_commit(store: &AssetStore, key: &ResourceKey, data: &[u8]) {
    let AcquisitionResult::Pending(w) = store
        .acquire_resource(key, None)
        .expect("test acquire must succeed")
    else {
        panic!("fresh acquire must be Pending");
    };
    w.write_at(0, data).expect("test write must succeed");
    w.commit(Some(data.len() as u64))
        .expect("test commit must succeed");
}

/// Commit `count` resources under `root` to force LRU displacement.
fn fill_root(store: &AssetStore, root: &str, count: usize) -> Vec<ResourceKey> {
    let scope = store.scope(root);
    let keys: Vec<ResourceKey> = (0..count)
        .map(|i| scope.key(format!("seg_{i}.m4s")))
        .collect();
    for key in &keys {
        write_commit(store, key, b"data");
    }
    keys
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn evicted_key_under_subscribed_root_is_delivered() {
    let store = ephemeral_store(2);

    let (tx, mut rx) = mpsc::unbounded_channel::<ResourceKey>();
    let _guard = store.subscribe_eviction(Arc::from(ROOT_A), tx);

    // The 2-slot LRU displaces the oldest key on the third insert; its
    // bytes are gone (ephemeral), so the subscriber must receive it.
    let keys = fill_root(&store, ROOT_A, 3);

    let received = rx.try_recv().expect("evicted key must be delivered");
    assert_eq!(received, keys[0]);
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn evicted_key_under_other_root_is_not_delivered() {
    let store = ephemeral_store(2);

    let (tx, mut rx) = mpsc::unbounded_channel::<ResourceKey>();
    let _guard = store.subscribe_eviction(Arc::from(ROOT_A), tx);

    // Evict under a DIFFERENT root; the ROOT_A subscriber must see nothing.
    let _keys = fill_root(&store, ROOT_B, 3);

    assert!(
        rx.try_recv().is_err(),
        "a key under a different asset_root must not reach this subscriber"
    );
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn dropping_guard_deregisters() {
    let store = ephemeral_store(2);

    let (tx, mut rx) = mpsc::unbounded_channel::<ResourceKey>();
    let guard = store.subscribe_eviction(Arc::from(ROOT_A), tx);
    drop(guard);

    let _keys = fill_root(&store, ROOT_A, 3);

    assert!(
        rx.try_recv().is_err(),
        "after dropping the guard no eviction may be delivered"
    );
}
