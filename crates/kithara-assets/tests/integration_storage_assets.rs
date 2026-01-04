#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_assets::{EvictConfig, ResourceKey, asset_store};
use kithara_storage::{Resource, StreamingResourceExt};
use tokio_util::sync::CancellationToken;

fn mp3_bytes() -> Bytes {
    // Deterministic “mp3-like” payload (we don't parse mp3 here; just bytes).
    // Big enough to exercise read/write paths.
    let mut v = Vec::with_capacity(128 * 1024);
    for i in 0..v.capacity() {
        v.push((i % 251) as u8);
    }
    Bytes::from(v)
}

fn read_pins_file(root: &std::path::Path) -> serde_json::Value {
    let path = root.join("_index").join("pins.json");
    let bytes = std::fs::read(&path).expect("pins index file must exist on disk");
    serde_json::from_slice(&bytes).expect("pins index must be valid json")
}

#[tokio::test]
async fn mp3_single_file_atomic_roundtrip_with_pins_persisted() {
    let dir = tempfile::tempdir().unwrap();

    let store = asset_store(
        dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    );

    let cancel = CancellationToken::new();

    // MP3 scenario: single wrapped file inside an asset.
    let key = ResourceKey::new("asset-mp3-001", "media/audio.mp3");
    let payload = mp3_bytes();

    // Keep the handle alive while we check the persisted pins file.
    let res = store
        .open_atomic_resource(&key, cancel.clone())
        .await
        .unwrap();

    res.write(&payload).await.unwrap();
    res.commit(None).await.unwrap();

    let read_back = res.read().await.unwrap();
    assert_eq!(read_back, payload);

    // Pins are persisted immediately; verify without assuming any `ResourceKey`/meta API.
    let pins_json = read_pins_file(dir.path());
    let pinned = pins_json
        .get("pinned")
        .and_then(|v| v.as_array())
        .expect("pins index must contain `pinned` array");

    assert!(
        pinned.iter().any(|v| v.as_str() == Some("asset-mp3-001")),
        "mp3 asset_root must be pinned while resource is open (and persisted immediately)"
    );

    drop(res);
}

#[tokio::test]
async fn hls_multi_file_streaming_and_atomic_roundtrip_with_pins_persisted() {
    let dir = tempfile::tempdir().unwrap();

    let store = asset_store(
        dir.path(),
        EvictConfig {
            max_assets: None,
            max_bytes: None,
        },
    );

    let cancel = CancellationToken::new();

    // HLS scenario: many resources under one asset_root.
    let asset_root = "asset-hls-123";

    // 1) playlist (atomic)
    let playlist_key = ResourceKey::new(asset_root, "master.m3u8");
    let playlist_bytes = Bytes::from_static(b"#EXTM3U\n#EXT-X-VERSION:7\n");

    let playlist = store
        .open_atomic_resource(&playlist_key, cancel.clone())
        .await
        .unwrap();
    playlist.write(&playlist_bytes).await.unwrap();
    playlist.commit(None).await.unwrap();
    assert_eq!(playlist.read().await.unwrap(), playlist_bytes);

    // 2) segments (streaming, random access writes)
    let seg1_key = ResourceKey::new(asset_root, "segments/0001.m4s");
    let seg2_key = ResourceKey::new(asset_root, "segments/0002.m4s");

    let seg1 = store
        .open_streaming_resource(&seg1_key, cancel.clone())
        .await
        .unwrap();
    let seg2 = store
        .open_streaming_resource(&seg2_key, cancel.clone())
        .await
        .unwrap();

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

    // seg2: single contiguous write
    let c = vec![0xCCu8; 10 * 1024];
    seg2.write_at(0, &c).await.unwrap();
    seg2.wait_range(0..(c.len() as u64)).await.unwrap();
    assert_eq!(seg2.read_at(0, c.len()).await.unwrap(), Bytes::from(c));

    // Seal resources (optional but makes the lifecycle explicit).
    seg1.commit(None).await.unwrap();
    seg2.commit(None).await.unwrap();

    // Pins are persisted immediately; verify while handles are still alive.
    let pins_json = read_pins_file(dir.path());
    let pinned = pins_json
        .get("pinned")
        .and_then(|v| v.as_array())
        .expect("pins index must contain `pinned` array");

    assert!(
        pinned.iter().any(|v| v.as_str() == Some(asset_root)),
        "hls asset_root must be pinned while any resource is open (and persisted immediately)"
    );

    drop(seg2);
    drop(seg1);
    drop(playlist);
}
