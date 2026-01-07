#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_assets::{AssetStore, EvictConfig, ResourceKey};
use kithara_storage::Resource;
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

fn exists_asset_dir(root: &std::path::Path, asset_root: &str) -> bool {
    root.join(asset_root).exists()
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
fn asset_store_with_100_bytes_limit(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: Some(100),
        },
    )
}

#[rstest]
#[tokio::test]
#[ignore = "eviction logic needs investigation"]
async fn eviction_max_bytes_uses_explicit_touch_asset_bytes(
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_with_100_bytes_limit: AssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let store = asset_store_with_100_bytes_limit;

    // Keep bytes under 100. We'll:
    // - create A (60 bytes)
    // - create B (60 bytes)
    // - then create C which triggers eviction at "asset creation time"
    //
    // With max_bytes=100, we must evict the oldest non-pinned asset(s) until we're <= 100.
    // Since A is oldest and not pinned, it should be evicted.

    // Asset A: create some data and then explicitly record bytes in LRU via eviction decorator.
    {
        let key_a = ResourceKey {
            asset_root: "asset-a".to_string(),
            rel_path: "media/a.bin".to_string(),
        };
        let res_a = store
            .open_atomic_resource(&key_a, cancel.clone())
            .await
            .unwrap();

        res_a.write(&Bytes::from(vec![0xAAu8; 60])).await.unwrap();
        res_a.commit(None).await.unwrap();

        // Explicit bytes accounting (MVP for max_bytes):
        // `AssetStore` is `LeaseAssets<EvictAssets<DiskAssetStore>>`, so we can reach `EvictAssets`
        // via `.base()`.
        store
            .base()
            .touch_asset_bytes("asset-a", 60, cancel.clone())
            .await
            .unwrap();

        assert!(exists_asset_dir(dir, "asset-a"));
    }

    // Asset B: create and record bytes.
    {
        let key_b = ResourceKey {
            asset_root: "asset-b".to_string(),
            rel_path: "media/b.bin".to_string(),
        };
        let res_b = store
            .open_atomic_resource(&key_b, cancel.clone())
            .await
            .unwrap();

        res_b.write(&Bytes::from(vec![0xBBu8; 60])).await.unwrap();
        res_b.commit(None).await.unwrap();

        store
            .base()
            .touch_asset_bytes("asset-b", 60, cancel.clone())
            .await
            .unwrap();

        assert!(exists_asset_dir(dir, "asset-b"));
    }

    // Asset C: first open for a new asset_root triggers eviction.
    {
        let key_c = ResourceKey {
            asset_root: "asset-c".to_string(),
            rel_path: "media/c.bin".to_string(),
        };
        let res_c = store
            .open_atomic_resource(&key_c, cancel.clone())
            .await
            .unwrap();

        res_c.write(&Bytes::from_static(b"C")).await.unwrap();
        res_c.commit(None).await.unwrap();

        assert!(exists_asset_dir(dir, "asset-c"));
    }

    // Expect A evicted (oldest) to satisfy max_bytes.
    assert!(
        !exists_asset_dir(dir, "asset-a"),
        "asset-a should be evicted as the oldest asset to satisfy max_bytes"
    );
    assert!(
        exists_asset_dir(dir, "asset-b"),
        "asset-b should remain after eviction"
    );
    assert!(
        exists_asset_dir(dir, "asset-c"),
        "asset-c is newly created and should exist"
    );
}
