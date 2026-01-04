#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_assets::{EvictConfig, ResourceKey, asset_store};
use kithara_storage::Resource;
use tokio_util::sync::CancellationToken;

fn exists_asset_dir(root: &std::path::Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

fn read_pins_file(root: &std::path::Path) -> serde_json::Value {
    let path = root.join("_index").join("pins.json");
    let bytes = std::fs::read(&path).expect("pins index file must exist on disk");
    serde_json::from_slice(&bytes).expect("pins index must be valid json")
}

#[tokio::test]
async fn eviction_max_assets_skips_pinned_assets() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();

    // Keep only 2 assets. We'll create 3 (A, B, C) and keep B pinned by holding a handle.
    let store = asset_store(
        dir.path(),
        EvictConfig {
            max_assets: Some(2),
            max_bytes: None,
        },
    );

    // Asset A: create a file and drop handle => not pinned afterwards.
    {
        let key_a = ResourceKey::new("asset-a", "media/a.bin");
        let res_a = store
            .open_atomic_resource(&key_a, cancel.clone())
            .await
            .unwrap();
        res_a.write(&Bytes::from_static(b"AAAA")).await.unwrap();
        res_a.commit(None).await.unwrap();
        assert!(exists_asset_dir(dir.path(), "asset-a"));
    }

    // Asset B: create and KEEP handle alive => pinned while we create C.
    let key_b = ResourceKey::new("asset-b", "media/b.bin");
    let res_b = store
        .open_atomic_resource(&key_b, cancel.clone())
        .await
        .unwrap();
    res_b.write(&Bytes::from_static(b"BBBB")).await.unwrap();
    res_b.commit(None).await.unwrap();
    assert!(exists_asset_dir(dir.path(), "asset-b"));

    // Sanity: pins file should contain asset-b while handle is alive.
    let pins_json = read_pins_file(dir.path());
    let pinned = pins_json
        .get("pinned")
        .and_then(|v| v.as_array())
        .expect("pins index must contain `pinned` array");
    assert!(
        pinned.iter().any(|v| v.as_str() == Some("asset-b")),
        "asset-b must be pinned while its handle is alive"
    );

    // Asset C: first open for new asset_root should trigger eviction (max_assets=2).
    // The oldest non-pinned (asset-a) should be evicted; pinned asset-b must stay.
    {
        let key_c = ResourceKey::new("asset-c", "media/c.bin");
        let res_c = store
            .open_atomic_resource(&key_c, cancel.clone())
            .await
            .unwrap();
        res_c.write(&Bytes::from_static(b"CCCC")).await.unwrap();
        res_c.commit(None).await.unwrap();
        assert!(exists_asset_dir(dir.path(), "asset-c"));
    }

    assert!(
        !exists_asset_dir(dir.path(), "asset-a"),
        "asset-a should be evicted as the oldest non-pinned asset"
    );
    assert!(
        exists_asset_dir(dir.path(), "asset-b"),
        "asset-b must NOT be evicted because it is pinned"
    );
    assert!(
        exists_asset_dir(dir.path(), "asset-c"),
        "asset-c is the newly created asset and must exist"
    );

    // Drop pinned handle at the end.
    drop(res_b);
}
