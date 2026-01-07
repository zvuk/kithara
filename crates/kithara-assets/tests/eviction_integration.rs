#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_assets::{AssetStore, EvictConfig, ResourceKey};
use kithara_storage::Resource;
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

fn exists_asset_dir(root: &std::path::Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
}

fn read_pins_file(root: &std::path::Path) -> serde_json::Value {
    let path = root.join("_index").join("pins.json");
    let bytes = std::fs::read(&path).expect("pins index file must exist on disk");
    serde_json::from_slice(&bytes).expect("pins index must be valid json")
}

#[fixture]
fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

#[fixture]
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[fixture]
fn asset_store_with_max_2_assets(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: Some(2),
            max_bytes: None,
        },
    )
}

#[rstest]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_max_assets_skips_pinned_assets(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_with_max_2_assets: AssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let store = asset_store_with_max_2_assets;

    // Keep only 2 assets. We'll create 3 (A, B, C) and keep B pinned by holding a handle.

    // Asset A: create a file and drop handle => not pinned afterwards.
    {
        let key_a = ResourceKey {
            asset_root: "asset-a".to_string(),
            rel_path: "media/a.bin".to_string(),
        };
        let res_a = store
            .open_atomic_resource(&key_a, cancel.clone())
            .await
            .unwrap();
        res_a.write(&Bytes::from_static(b"AAAA")).await.unwrap();
        res_a.commit(None).await.unwrap();
        assert!(exists_asset_dir(dir, "asset-a"));
    }

    // Asset B: create and KEEP handle alive => pinned while we create C.
    let key_b = ResourceKey {
        asset_root: "asset-b".to_string(),
        rel_path: "media/b.bin".to_string(),
    };
    let res_b = store
        .open_atomic_resource(&key_b, cancel.clone())
        .await
        .unwrap();
    res_b.write(&Bytes::from_static(b"BBBB")).await.unwrap();
    res_b.commit(None).await.unwrap();
    assert!(exists_asset_dir(dir, "asset-b"));

    // Sanity: pins file should contain asset-b while handle is alive.
    let pins_json = read_pins_file(dir);
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
        let key_c = ResourceKey {
            asset_root: "asset-c".to_string(),
            rel_path: "media/c.bin".to_string(),
        };
        let res_c = store
            .open_atomic_resource(&key_c, cancel.clone())
            .await
            .unwrap();
        res_c.write(&Bytes::from_static(b"CCCC")).await.unwrap();
        res_c.commit(None).await.unwrap();
        assert!(exists_asset_dir(dir, "asset-c"));
    }

    assert!(
        !exists_asset_dir(dir, "asset-a"),
        "asset-a should be evicted as the oldest non-pinned asset"
    );
    assert!(
        exists_asset_dir(dir, "asset-b"),
        "asset-b must NOT be evicted because it is pinned"
    );
    assert!(
        exists_asset_dir(dir, "asset-c"),
        "asset-c is the newly created asset and must exist"
    );

    // Drop pinned handle at the end.
    drop(res_b);
}

#[rstest]
#[case(1, 2, 3)] // Test different max_assets values
#[case(2, 3, 4)]
#[case(3, 4, 5)]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_respects_max_assets_limit(
    #[case] max_assets: usize,
    #[case] create_count: usize,
    #[case] expected_remaining: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;

    let store = AssetStore::with_root_dir(
        dir,
        EvictConfig {
            max_assets: Some(max_assets),
            max_bytes: None,
        },
    );

    // Create more assets than the limit
    let mut handles = Vec::new();
    for i in 0..create_count {
        let key = ResourceKey {
            asset_root: format!("asset-{}", i),
            rel_path: format!("media/{}.bin", i),
        };
        let res = store
            .open_atomic_resource(&key, cancel.clone())
            .await
            .unwrap();
        res.write(&Bytes::from_static(b"DATA")).await.unwrap();
        res.commit(None).await.unwrap();

        // Keep handle for first expected_remaining assets to pin them
        if i < expected_remaining {
            handles.push(res);
        }
    }

    // Count how many asset directories exist
    let mut existing_count = 0;
    for i in 0..create_count {
        if exists_asset_dir(dir, &format!("asset-{}", i)) {
            existing_count += 1;
        }
    }

    // Should have at most max_assets remaining
    assert!(existing_count <= max_assets);

    // First expected_remaining assets should exist (pinned)
    for i in 0..expected_remaining {
        assert!(
            exists_asset_dir(dir, &format!("asset-{}", i)),
            "Asset {} should exist (pinned)",
            i
        );
    }
}
