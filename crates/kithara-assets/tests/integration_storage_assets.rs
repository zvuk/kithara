#![forbid(unsafe_code)]

use std::time::Duration;

use bytes::Bytes;
use kithara_assets::{AssetStore, EvictConfig, ResourceKey};
use kithara_storage::{Resource, StreamingResourceExt};
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

#[allow(dead_code)]
fn mp3_bytes() -> Bytes {
    // Deterministic “mp3-like” payload (we don't parse mp3 here; just bytes).
    // Big enough to exercise read/write paths.
    let mut v = Vec::with_capacity(128 * 1024);
    for i in 0..v.capacity() {
        v.push((i % 251) as u8);
    }
    Bytes::from(v)
}

fn read_pins_file(root: &std::path::Path) -> Option<serde_json::Value> {
    let path = root.join("_index").join("pins.json");
    if !path.exists() {
        return None;
    }
    let bytes = std::fs::read(&path).expect("pins index file should be readable if exists");
    Some(serde_json::from_slice(&bytes).expect("pins index must be valid json if exists"))
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
fn asset_store_no_limits(temp_dir: tempfile::TempDir) -> AssetStore {
    AssetStore::with_root_dir(
        temp_dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    )
}

#[rstest]
#[case("asset-mp3-001", "media/audio.mp3", 128 * 1024)]
#[case("asset-mp3-002", "audio/song.mp3", 64 * 1024)]
#[case("asset-mp3-003", "music/track.mp3", 256 * 1024)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn mp3_single_file_atomic_roundtrip_with_pins_persisted(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] size: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    // MP3 scenario: single wrapped file inside an asset.
    let key = ResourceKey {
        asset_root: asset_root.to_string(),
        rel_path: rel_path.to_string(),
    };
    let payload = Bytes::from((0..size).map(|i| (i % 251) as u8).collect::<Vec<_>>());

    // Keep the handle alive while we check the persisted pins file.
    let res = store
        .open_atomic_resource(&key, cancel.clone())
        .await
        .unwrap();

    res.write(&payload).await.unwrap();
    res.commit(None).await.unwrap();

    let read_back = res.read().await.unwrap();
    assert_eq!(read_back, payload);

    // Pins may be persisted; check if pins file exists
    if let Some(pins_json) = read_pins_file(dir) {
        let pinned = pins_json
            .get("pinned")
            .and_then(|v| v.as_array())
            .expect("pins index must contain `pinned` array if exists");

        assert!(
            pinned.iter().any(|v| v.as_str() == Some(asset_root)),
            "mp3 asset_root {} must be pinned while resource is open if pins file exists",
            asset_root
        );
    }

    drop(res);
}

#[rstest]
#[case("asset-hls-123", 3)]
#[case("asset-hls-456", 5)]
#[case("asset-hls-789", 2)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_multi_file_streaming_and_atomic_roundtrip_with_pins_persisted(
    #[case] asset_root: &str,
    #[case] segment_count: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let dir = temp_dir.path();
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    // HLS scenario: many resources under one asset_root.

    // 1) playlist (atomic)
    let playlist_key = ResourceKey {
        asset_root: asset_root.to_string(),
        rel_path: "master.m3u8".to_string(),
    };
    let playlist_bytes = Bytes::from_static(b"#EXTM3U\n#EXT-X-VERSION:7\n");

    let playlist = store
        .open_atomic_resource(&playlist_key, cancel.clone())
        .await
        .unwrap();
    playlist.write(&playlist_bytes).await.unwrap();
    playlist.commit(None).await.unwrap();
    assert_eq!(playlist.read().await.unwrap(), playlist_bytes);

    // 2) segments (streaming, random access writes)
    let mut segments = Vec::new();
    for i in 0..segment_count.min(2) {
        let seg_key = ResourceKey {
            asset_root: asset_root.to_string(),
            rel_path: format!("segments/{:04}.m4s", i + 1),
        };

        let seg = store
            .open_streaming_resource(&seg_key, cancel.clone())
            .await
            .unwrap();
        segments.push((seg, i));
    }

    if let Some((seg1, _)) = segments.get(0) {
        // Write two disjoint ranges in seg1 and read back.
        let a = vec![0xAAu8; 4096];
        let b = vec![0xBBu8; 2048];

        seg1.write_at(0, &a).await.unwrap();
        seg1.write_at(8192, &b).await.unwrap();

        // Ensure ranges become available before reading.
        seg1.wait_range(0..(a.len() as u64)).await.unwrap();
        seg1.wait_range(8192..(8192 + b.len() as u64))
            .await
            .unwrap();

        assert_eq!(seg1.read_at(0, a.len()).await.unwrap(), Bytes::from(a));
        assert_eq!(seg1.read_at(8192, b.len()).await.unwrap(), Bytes::from(b));
    }

    if let Some((seg2, _)) = segments.get(1) {
        // seg2: single contiguous write
        let c = vec![0xCCu8; 10 * 1024];
        seg2.write_at(0, &c).await.unwrap();
        seg2.wait_range(0..(c.len() as u64)).await.unwrap();
        assert_eq!(seg2.read_at(0, c.len()).await.unwrap(), Bytes::from(c));
    }

    // Seal resources (optional but makes the lifecycle explicit).
    for (seg, _) in &segments {
        seg.commit(None).await.unwrap();
    }

    // Pins may be persisted; check if pins file exists
    if let Some(pins_json) = read_pins_file(dir) {
        let pinned = pins_json
            .get("pinned")
            .and_then(|v| v.as_array())
            .expect("pins index must contain `pinned` array if exists");

        assert!(
            pinned.iter().any(|v| v.as_str() == Some(asset_root)),
            "hls asset_root must be pinned while any resource is open if pins file exists"
        );
    }

    for (seg, _) in segments {
        drop(seg);
    }
    drop(playlist);
}

#[rstest]
#[case("asset-test-1", "media/file1.bin")]
#[case("asset-test-2", "deep/path/to/file2.bin")]
#[case("asset-test-3", "file3.bin")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_roundtrip_with_different_paths(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey {
        asset_root: asset_root.to_string(),
        rel_path: rel_path.to_string(),
    };
    let payload = Bytes::from_static(b"test data for atomic resource");

    let res = store
        .open_atomic_resource(&key, cancel.clone())
        .await
        .unwrap();

    res.write(&payload).await.unwrap();
    res.commit(None).await.unwrap();

    let read_back = res.read().await.unwrap();
    assert_eq!(read_back, payload);
}

#[rstest]
#[case(0, 4096, 4096)] // Write at beginning
#[case(8192, 2048, 2048)] // Write at offset
#[case(16384, 10240, 10240)] // Larger write
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_write_read_at_different_positions(
    #[case] offset: u64,
    #[case] size: usize,
    #[case] read_size: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey {
        asset_root: "streaming-test".to_string(),
        rel_path: "data.bin".to_string(),
    };

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Create test data
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    // Write at specified offset
    res.write_at(offset, &data).await.unwrap();
    res.wait_range(offset..(offset + size as u64))
        .await
        .unwrap();

    // Read back
    let read_back = res.read_at(offset, read_size.min(size)).await.unwrap();
    assert_eq!(
        read_back,
        Bytes::copy_from_slice(&data[..read_size.min(size)])
    );

    res.commit(None).await.unwrap();
}

#[rstest]
#[case(2)]
#[case(3)]
#[case(5)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn multiple_resources_same_asset_root_independently_accessible(
    #[case] resource_count: usize,
    cancel_token: CancellationToken,
    temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let asset_root = "multi-resource-asset";

    // Create multiple resources under same asset_root
    let keys: Vec<ResourceKey> = (0..resource_count)
        .map(|i| ResourceKey {
            asset_root: asset_root.to_string(),
            rel_path: if i % 2 == 0 {
                format!("file{}.bin", i)
            } else {
                format!("subdir/file{}.bin", i)
            },
        })
        .collect();

    let mut resources = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let res = store
            .open_atomic_resource(key, cancel.clone())
            .await
            .unwrap();

        let data = Bytes::from(format!("data for file {}", i));
        res.write(&data).await.unwrap();
        res.commit(None).await.unwrap();

        resources.push((res, data));
    }

    // Verify each resource independently
    for (res, expected_data) in resources {
        let read_back = res.read().await.unwrap();
        assert_eq!(read_back, expected_data);
    }
}
