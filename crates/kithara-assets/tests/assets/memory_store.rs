#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use kithara_assets::{AssetResourceState, AssetStore, StorageBackend, WriteSide};
use kithara_test_utils::{bufpool::pools, kithara};

use super::support::{LiteralLayout, literal_layouts, pending, read_bytes, resource, source};

#[kithara::test]
fn retained_resource_survives_memory_cache_displacement() {
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(2).unwrap())
        .layouts(literal_layouts())
        .build();
    let scope = store
        .scope::<LiteralLayout>(&source("memory-asset"))
        .expect("scope");

    let retained_key = scope.key(&resource("retained.bin")).unwrap();
    let retained = pending(store.acquire_resource(&retained_key, None).unwrap()).retain();
    retained.write_at(0, b"keep").unwrap();
    drop(retained.commit(Some(4)).unwrap());

    let keys: Vec<_> = (0..3)
        .map(|i| scope.key(&resource(format!("seg_{i}.bin"))).unwrap())
        .collect();
    for (i, key) in keys.iter().enumerate() {
        let writer = pending(store.acquire_resource(key, None).unwrap());
        writer.write_at(0, format!("seg{i}").as_bytes()).unwrap();
        drop(writer.commit(Some(4)).unwrap());
    }

    assert!(
        store.open_resource(&keys[0], None).is_err(),
        "the oldest unretained resource must be displaced"
    );
    let reader = store.open_resource(&retained_key, None).unwrap();
    assert_eq!(read_bytes(&reader, 0, 4), b"keep");
    let reader = store.open_resource(&keys[1], None).unwrap();
    assert_eq!(
        read_bytes(&reader, 0, 4),
        b"seg1",
        "the retained resource must not take a slot from unretained resources"
    );
}

#[kithara::test]
fn memory_store_keeps_assets_past_max_assets() {
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .max_assets(1)
        .layouts(literal_layouts())
        .build();
    let key_a = store
        .scope::<LiteralLayout>(&source("asset-a"))
        .expect("scope")
        .key(&resource("seg.bin"))
        .unwrap();
    let key_b = store
        .scope::<LiteralLayout>(&source("asset-b"))
        .expect("scope")
        .key(&resource("seg.bin"))
        .unwrap();

    for (key, data) in [(&key_a, b"aaaa"), (&key_b, b"bbbb")] {
        let writer = pending(store.acquire_resource(key, None).unwrap());
        writer.write_at(0, data).unwrap();
        drop(writer.commit(Some(4)).unwrap());
    }

    assert_eq!(
        store.final_len(&key_a),
        Some(4),
        "the memory store must not delete an asset over the asset quota"
    );
    let reader = store.open_resource(&key_a, None).unwrap();
    assert_eq!(read_bytes(&reader, 0, 4), b"aaaa");
}

#[kithara::test]
fn memory_store_reports_committed_state_after_handles_drop() {
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .layouts(literal_layouts())
        .build();
    let scope = store
        .scope::<LiteralLayout>(&source("memory-asset"))
        .expect("scope");
    let committed = scope.key(&resource("committed.bin")).unwrap();
    let in_flight = scope.key(&resource("in-flight.bin")).unwrap();

    let writer = pending(store.acquire_resource(&committed, None).unwrap());
    writer.write_at(0, b"done").unwrap();
    drop(writer.commit(Some(4)).unwrap());

    let sibling = pending(store.acquire_resource(&in_flight, None).unwrap());
    sibling.write_at(0, b"part").unwrap();

    assert_eq!(
        store.resource_state(&committed).unwrap(),
        AssetResourceState::Committed { final_len: Some(4) }
    );
    drop(sibling);
}
