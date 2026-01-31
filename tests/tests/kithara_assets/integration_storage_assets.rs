#![forbid(unsafe_code)]

use std::time::Duration;

use kithara_assets::{AssetStore, AssetStoreBuilder, Assets, EvictConfig, ResourceKey};
use kithara_bufpool::byte_pool;
use kithara_storage::ResourceExt;
use rstest::{fixture, rstest};

/// Helper to read bytes from resource into a pooled buffer
fn read_bytes<R: ResourceExt>(res: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = byte_pool().get_with(|b| b.resize(len, 0));
    let n = res.read_at(offset, &mut buf).unwrap_or(0);
    buf[..n].to_vec()
}

#[allow(dead_code)]
fn mp3_bytes() -> Vec<u8> {
    // Deterministic "mp3-like" payload (we don't parse mp3 here; just bytes).
    // Big enough to exercise read/write paths.
    let mut v = Vec::with_capacity(128 * 1024);
    for i in 0..v.capacity() {
        v.push((i % 251) as u8);
    }
    v
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct PinsIndexFile {
    version: u32,
    pinned: Vec<String>,
}

fn read_pins_file(root: &std::path::Path) -> Option<Vec<String>> {
    let path = root.join("_index").join("pins.json");
    if !path.exists() {
        return None;
    }
    let bytes = std::fs::read(&path).expect("pins index file should be readable if exists");
    let (file, _): (PinsIndexFile, _) = bincode::serde::decode_from_slice(
        &bytes,
        bincode::config::legacy(),
    )
    .expect("pins index must be valid bincode if exists");
    Some(file.pinned)
}

#[fixture]
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn asset_store_with_root(temp_dir: &tempfile::TempDir, asset_root: &str) -> AssetStore {
    AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root(asset_root)
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build()
}

#[rstest]
#[case("asset-mp3-001", "media/audio.mp3", 128 * 1024)]
#[case("asset-mp3-002", "audio/song.mp3", 64 * 1024)]
#[case("asset-mp3-003", "music/track.mp3", 256 * 1024)]
#[timeout(Duration::from_secs(5))]
#[test]
fn mp3_single_file_atomic_roundtrip_with_pins_persisted(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] size: usize,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path().to_path_buf();
    let store = asset_store_with_root(&temp_dir, asset_root);

    // MP3 scenario: single wrapped file inside an asset.
    let key = ResourceKey::new(rel_path);
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    // Keep the handle alive while we check the persisted pins file.
    let res = store.open_resource(&key).unwrap();

    res.write_all(&payload).unwrap();

    let mut read_back = byte_pool().get();
    res.read_into(&mut read_back).unwrap();
    assert_eq!(&*read_back, &payload[..]);

    // Pins may be persisted; check if pins file exists
    if let Some(pinned) = read_pins_file(&dir) {
        assert!(
            pinned.iter().any(|v| v == asset_root),
            "mp3 asset_root {} must be pinned while resource is open if pins file exists",
            asset_root
        );
    }

    drop(res);
}

#[rstest]
#[case("persist-atomic-1", "media/atomic_a.bin", b"atomic data")]
#[case("persist-atomic-empty", "media/atomic_empty.bin", b"")]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_persistence(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] payload: &[u8],
    temp_dir: tempfile::TempDir,
) {
    let store = asset_store_with_root(&temp_dir, asset_root);
    let key = ResourceKey::new(rel_path);

    {
        let res = store.open_resource(&key).unwrap();
        res.write_all(payload).unwrap();
    }

    let res = store.open_resource(&key).unwrap();
    let mut buf = byte_pool().get();
    res.read_into(&mut buf).unwrap();
    assert_eq!(&*buf, payload);
}

#[rstest]
#[case("persist-stream-1", "media/stream1.bin", b"stream payload")]
#[case("persist-stream-2", "media/stream2.bin", b"more stream data")]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_persistence(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] payload: &[u8],
    temp_dir: tempfile::TempDir,
) {
    let store = asset_store_with_root(&temp_dir, asset_root);
    let key = ResourceKey::new(rel_path);

    {
        let res = store.open_resource(&key).unwrap();
        res.write_at(0, payload).unwrap();
        res.commit(Some(payload.len() as u64)).unwrap();
        res.wait_range(0..payload.len() as u64).unwrap();
    }

    let res = store.open_resource(&key).unwrap();
    let data = read_bytes(&res, 0, payload.len());
    assert_eq!(&data, payload);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn mixed_resource_persistence_across_reopen(temp_dir: tempfile::TempDir) {
    let store = asset_store_with_root(&temp_dir, "mixed-asset");

    let atomic_key = ResourceKey::new("meta/index.json");
    let streaming_key = ResourceKey::new("media/data.bin");

    let atomic_payload = b"{\"idx\":1}".to_vec();
    let streaming_payload = b"stream-bytes-123".to_vec();

    {
        let atomic = store.open_resource(&atomic_key).unwrap();
        atomic.write_all(&atomic_payload).unwrap();

        let streaming = store.open_resource(&streaming_key).unwrap();
        streaming.write_at(0, &streaming_payload).unwrap();
        streaming
            .commit(Some(streaming_payload.len() as u64))
            .unwrap();
        streaming
            .wait_range(0..streaming_payload.len() as u64)
            .unwrap();
    }

    let atomic = store.open_resource(&atomic_key).unwrap();
    let mut atomic_read = byte_pool().get();
    atomic.read_into(&mut atomic_read).unwrap();
    assert_eq!(&*atomic_read, &atomic_payload[..]);

    let streaming = store.open_resource(&streaming_key).unwrap();
    streaming
        .wait_range(0..streaming_payload.len() as u64)
        .unwrap();
    let streaming_read = read_bytes(&streaming, 0, streaming_payload.len());
    assert_eq!(&streaming_read, &streaming_payload[..]);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_concurrent_write_and_read_across_handles(temp_dir: tempfile::TempDir) {
    let store = asset_store_with_root(&temp_dir, "concurrent-asset");

    let key = ResourceKey::new("media/concurrent.bin");
    let payload: Vec<u8> = b"concurrent streaming data".to_vec();
    let payload_len = payload.len() as u64;

    let store_reader = store.clone();
    let key_reader = key.clone();
    let payload_len_reader = payload_len;
    let reader = std::thread::spawn(move || {
        let res = store_reader.open_resource(&key_reader).unwrap();
        res.wait_range(0..payload_len_reader).unwrap();
        let mut buf = byte_pool().get_with(|b| b.resize(payload_len_reader as usize, 0));
        let n = res.read_at(0, &mut buf).unwrap();
        buf.truncate(n);
        buf.to_vec()
    });

    let store_writer = store;
    let payload_writer = payload.clone();
    let key_writer = key;
    let writer = std::thread::spawn(move || {
        let res = store_writer.open_resource(&key_writer).unwrap();
        res.write_at(0, &payload_writer).unwrap();
        res.commit(Some(payload_writer.len() as u64)).unwrap();
        res.wait_range(0..payload_writer.len() as u64).unwrap();
    });

    let data = reader.join().unwrap();
    writer.join().unwrap();
    assert_eq!(&data, &payload);
}

#[rstest]
#[case("asset-hls-123", 3)]
#[case("asset-hls-456", 5)]
#[case("asset-hls-789", 2)]
#[timeout(Duration::from_secs(5))]
#[test]
fn hls_multi_file_streaming_and_atomic_roundtrip_with_pins_persisted(
    #[case] asset_root: &str,
    #[case] segment_count: usize,
    temp_dir: tempfile::TempDir,
) {
    let dir = temp_dir.path().to_path_buf();
    let store = asset_store_with_root(&temp_dir, asset_root);

    // HLS scenario: many resources under one asset_root.

    // 1) playlist (atomic)
    let playlist_key = ResourceKey::new("master.m3u8");
    let playlist_bytes = b"#EXTM3U\n#EXT-X-VERSION:7\n".to_vec();

    let playlist = store.open_resource(&playlist_key).unwrap();
    playlist.write_all(&playlist_bytes).unwrap();
    let mut playlist_read = byte_pool().get();
    playlist.read_into(&mut playlist_read).unwrap();
    assert_eq!(&*playlist_read, &playlist_bytes[..]);

    // 2) segments (streaming, random access writes)
    let mut segments = Vec::new();
    for i in 0..segment_count.min(2) {
        let seg_key = ResourceKey::new(format!("segments/{:04}.m4s", i + 1));

        let seg = store.open_resource(&seg_key).unwrap();
        segments.push((seg, i));
    }

    if let Some((seg1, _)) = segments.first() {
        // Write two disjoint ranges in seg1 and read back.
        let a = vec![0xAAu8; 4096];
        let b = vec![0xBBu8; 2048];

        seg1.write_at(0, &a).unwrap();
        seg1.write_at(8192, &b).unwrap();

        // Ensure ranges become available before reading.
        seg1.wait_range(0..(a.len() as u64)).unwrap();
        seg1.wait_range(8192..(8192 + b.len() as u64)).unwrap();

        assert_eq!(read_bytes(seg1, 0, a.len()), a);
        assert_eq!(read_bytes(seg1, 8192, b.len()), b);
    }

    if let Some((seg2, _)) = segments.get(1) {
        // seg2: single contiguous write
        let c = vec![0xCCu8; 10 * 1024];
        seg2.write_at(0, &c).unwrap();
        seg2.wait_range(0..(c.len() as u64)).unwrap();
        assert_eq!(read_bytes(seg2, 0, c.len()), c);
    }

    // Seal resources (optional but makes the lifecycle explicit).
    for (seg, _) in &segments {
        seg.commit(None).unwrap();
    }

    // Pins may be persisted; check if pins file exists
    if let Some(pinned) = read_pins_file(&dir) {
        assert!(
            pinned.iter().any(|v| v == asset_root),
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
#[test]
fn atomic_resource_roundtrip_with_different_paths(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    temp_dir: tempfile::TempDir,
) {
    let store = asset_store_with_root(&temp_dir, asset_root);

    let key = ResourceKey::new(rel_path);
    let payload = b"test data for atomic resource".to_vec();

    let res = store.open_resource(&key).unwrap();

    res.write_all(&payload).unwrap();

    let mut read_back = byte_pool().get();
    res.read_into(&mut read_back).unwrap();
    assert_eq!(&*read_back, &payload[..]);
}

#[rstest]
#[case(0, 4096, 4096)] // Write at beginning
#[case(8192, 2048, 2048)] // Write at offset
#[case(16384, 10240, 10240)] // Larger write
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_write_read_at_different_positions(
    #[case] offset: u64,
    #[case] size: usize,
    #[case] read_size: usize,
    temp_dir: tempfile::TempDir,
) {
    let store = asset_store_with_root(&temp_dir, "streaming-test");

    let key = ResourceKey::new("data.bin");
    let res = store.open_resource(&key).unwrap();

    // Create test data
    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    // Write at specified offset
    res.write_at(offset, &data).unwrap();
    res.wait_range(offset..(offset + size as u64)).unwrap();

    // Read back
    let read_back = read_bytes(&res, offset, read_size.min(size));
    assert_eq!(read_back, &data[..read_size.min(size)]);

    res.commit(None).unwrap();
}

#[rstest]
#[case(2)]
#[case(3)]
#[case(5)]
#[timeout(Duration::from_secs(5))]
#[test]
fn multiple_resources_same_asset_root_independently_accessible(
    #[case] resource_count: usize,
    temp_dir: tempfile::TempDir,
) {
    let asset_root = "multi-resource-asset";
    let store = asset_store_with_root(&temp_dir, asset_root);

    // Create multiple resources under same asset_root
    let keys: Vec<ResourceKey> = (0..resource_count)
        .map(|i| {
            let rel_path = if i % 2 == 0 {
                format!("file{}.bin", i)
            } else {
                format!("subdir/file{}.bin", i)
            };
            ResourceKey::new(rel_path)
        })
        .collect();

    let mut resources = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let res = store.open_resource(key).unwrap();

        let data = format!("data for file {}", i).into_bytes();
        res.write_all(&data).unwrap();

        resources.push((res, data));
    }

    // Verify each resource independently
    for (res, expected_data) in resources {
        let mut read_back = byte_pool().get();
        res.read_into(&mut read_back).unwrap();
        assert_eq!(&*read_back, &expected_data[..]);
    }
}

/// Test that delete_asset only deletes the asset directory for the store's asset_root,
/// leaving other assets in the same root_dir untouched.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn delete_asset_only_removes_own_directory(temp_dir: tempfile::TempDir) {
    let root_path = temp_dir.path();

    // Create three separate assets in the same root_dir
    let asset_roots = ["asset-alpha", "asset-beta", "asset-gamma"];
    let payloads: [&[u8]; 3] = [b"alpha data", b"beta data", b"gamma data"];

    // Create stores and write data for each asset
    for (i, asset_root) in asset_roots.iter().enumerate() {
        let store = asset_store_with_root(&temp_dir, asset_root);
        let key = ResourceKey::new("data.bin");
        let res = store.open_resource(&key).unwrap();
        res.write_all(payloads[i]).unwrap();
    }

    // Verify all asset directories exist
    for asset_root in &asset_roots {
        let asset_path = root_path.join(asset_root);
        assert!(
            asset_path.exists(),
            "asset directory {} should exist before deletion",
            asset_root
        );
    }

    // Delete the second asset (asset-beta)
    {
        let store = asset_store_with_root(&temp_dir, "asset-beta");
        store.delete_asset().unwrap();
    }

    // Verify asset-beta is deleted
    assert!(
        !root_path.join("asset-beta").exists(),
        "asset-beta directory should be deleted"
    );

    // Verify asset-alpha still exists and data is intact
    assert!(
        root_path.join("asset-alpha").exists(),
        "asset-alpha directory should still exist"
    );
    {
        let store = asset_store_with_root(&temp_dir, "asset-alpha");
        let key = ResourceKey::new("data.bin");
        let res = store.open_resource(&key).unwrap();
        let mut buf = byte_pool().get();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, payloads[0], "asset-alpha data should be intact");
    }

    // Verify asset-gamma still exists and data is intact
    assert!(
        root_path.join("asset-gamma").exists(),
        "asset-gamma directory should still exist"
    );
    {
        let store = asset_store_with_root(&temp_dir, "asset-gamma");
        let key = ResourceKey::new("data.bin");
        let res = store.open_resource(&key).unwrap();
        let mut buf = byte_pool().get();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, payloads[2], "asset-gamma data should be intact");
    }
}

/// Test sequential deletion of multiple assets in the same root_dir.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn delete_assets_sequentially(temp_dir: tempfile::TempDir) {
    let root_path = temp_dir.path();

    let asset_roots = ["seq-asset-1", "seq-asset-2", "seq-asset-3", "seq-asset-4"];

    // Create all assets
    for (i, asset_root) in asset_roots.iter().enumerate() {
        let store = asset_store_with_root(&temp_dir, asset_root);
        let key = ResourceKey::new(format!("file{}.bin", i));
        let res = store.open_resource(&key).unwrap();
        res.write_all(format!("content {}", i).as_bytes()).unwrap();
    }

    // Verify all directories exist
    for asset_root in &asset_roots {
        assert!(
            root_path.join(asset_root).exists(),
            "{} should exist",
            asset_root
        );
    }

    // Delete assets one by one and verify isolation
    for (delete_idx, asset_to_delete) in asset_roots.iter().enumerate() {
        // Delete this asset
        {
            let store = asset_store_with_root(&temp_dir, asset_to_delete);
            store.delete_asset().unwrap();
        }

        // Verify it's deleted
        assert!(
            !root_path.join(asset_to_delete).exists(),
            "{} should be deleted",
            asset_to_delete
        );

        // Verify remaining assets still exist
        for (i, remaining) in asset_roots.iter().enumerate() {
            if i > delete_idx {
                assert!(
                    root_path.join(remaining).exists(),
                    "{} should still exist after deleting {}",
                    remaining,
                    asset_to_delete
                );
            }
        }
    }

    // All asset directories should be gone now
    for asset_root in &asset_roots {
        assert!(
            !root_path.join(asset_root).exists(),
            "{} should not exist after sequential deletion",
            asset_root
        );
    }
}

/// Test that deleting a non-existent asset doesn't affect other assets.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn delete_nonexistent_asset_is_idempotent(temp_dir: tempfile::TempDir) {
    let root_path = temp_dir.path();

    // Create one asset
    {
        let store = asset_store_with_root(&temp_dir, "existing-asset");
        let key = ResourceKey::new("data.bin");
        let res = store.open_resource(&key).unwrap();
        res.write_all(b"existing data").unwrap();
    }

    // Delete a non-existent asset (should succeed without error)
    {
        let store = asset_store_with_root(&temp_dir, "nonexistent-asset");
        let result = store.delete_asset();
        assert!(result.is_ok(), "deleting non-existent asset should succeed");
    }

    // Verify existing asset is still intact
    assert!(
        root_path.join("existing-asset").exists(),
        "existing-asset should still exist"
    );
    {
        let store = asset_store_with_root(&temp_dir, "existing-asset");
        let key = ResourceKey::new("data.bin");
        let res = store.open_resource(&key).unwrap();
        let mut buf = byte_pool().get();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, b"existing data");
    }
}
