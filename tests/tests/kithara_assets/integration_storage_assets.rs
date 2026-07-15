#![forbid(unsafe_code)]

use std::{fs, path::Path};

use kithara::{
    assets::{
        AcquisitionResult, AssetScope, AssetStoreBuilder, EvictConfig, ReadSide, ResourceKey,
        StorageBackend, WriteSide,
        index::schema::{ArchivedPinsIndexFile, PinsIndexFile},
    },
    bufpool::BytePool,
    platform::{thread, time::Duration},
};
use kithara_integration_tests::temp_dir;

use super::support::{AssetScopeTestKeyExt, AssetStoreTestScopeExt, literal_layouts};

/// Helper to read bytes from resource into a pooled buffer
fn read_bytes<R: ReadSide>(res: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = BytePool::default().get_with(|b| b.resize(len, 0));
    let n = res.read_at(offset, &mut buf).unwrap_or(0);
    buf[..n].to_vec()
}

fn pending<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>) -> W {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer")
    };
    w
}

fn read_pins_file(root: &Path) -> Option<Vec<String>> {
    let path = root.join("_index").join("pins.bin");
    if !path.exists() {
        return None;
    }
    let bytes = fs::read(&path).expect("pins index file should be readable if exists");
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let archived = rkyv::access::<ArchivedPinsIndexFile, rkyv::rancor::Error>(&bytes)
        .expect("pins index must be valid rkyv if exists");

    let pins_file: PinsIndexFile =
        rkyv::deserialize::<PinsIndexFile, rkyv::rancor::Error>(archived).unwrap();

    let mut pinned = Vec::new();
    for (k, v) in &pins_file.pinned {
        if *v {
            pinned.push(k.to_string());
        }
    }
    Some(pinned)
}

fn asset_scope_with_root(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
) -> AssetScope {
    AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (temp_dir.path()).into(),
        })
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .layouts(literal_layouts())
        .build()
        .test_scope(asset_root)
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case("asset-mp3-001", "media/audio.mp3", 128 * 1024)]
#[case("asset-mp3-002", "audio/song.mp3", 64 * 1024)]
#[case("asset-mp3-003", "music/track.mp3", 256 * 1024)]
fn mp3_single_file_atomic_roundtrip_with_pins_persisted(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] size: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let dir = temp_dir.path().to_path_buf();
    let scope = asset_scope_with_root(&temp_dir, asset_root);

    let key = scope.test_key(rel_path);
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    let writer = pending(scope.store().acquire_resource(&key, None).unwrap());

    writer.write_at(0, &payload).unwrap();
    let res = writer.commit(Some(payload.len() as u64)).unwrap();

    let mut read_back = BytePool::default().get();
    res.read_into(&mut read_back).unwrap();
    assert_eq!(&*read_back, &payload[..]);

    if let Some(pinned) = read_pins_file(&dir) {
        assert!(
            pinned.iter().any(|v| v == asset_root),
            "mp3 asset_root {} must be pinned while resource is open if pins file exists",
            asset_root
        );
    }

    drop(res);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
#[case("persist-atomic-1", "media/atomic_a.bin", b"atomic data")]
#[case("persist-atomic-empty", "media/atomic_empty.bin", b"")]
fn atomic_resource_persistence(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] payload: &[u8],
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let scope = asset_scope_with_root(&temp_dir, asset_root);
    let key = scope.test_key(rel_path);

    {
        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        writer.write_at(0, payload).unwrap();
        writer.commit(Some(payload.len() as u64)).unwrap();
    }

    let res = scope.store().open_resource(&key, None).unwrap();
    let mut buf = BytePool::default().get();
    res.read_into(&mut buf).unwrap();
    assert_eq!(&*buf, payload);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
#[case("persist-stream-1", "media/stream1.bin", b"stream payload")]
#[case("persist-stream-2", "media/stream2.bin", b"more stream data")]
fn streaming_resource_persistence(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    #[case] payload: &[u8],
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let scope = asset_scope_with_root(&temp_dir, asset_root);
    let key = scope.test_key(rel_path);

    {
        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        writer.write_at(0, payload).unwrap();
        let res = writer.commit(Some(payload.len() as u64)).unwrap();
        res.wait_range(0..payload.len() as u64).unwrap();
    }

    let res = scope.store().open_resource(&key, None).unwrap();
    let data = read_bytes(&res, 0, payload.len());
    assert_eq!(&data, payload);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn mixed_resource_persistence_across_reopen(temp_dir: kithara_integration_tests::TestTempDir) {
    let scope = asset_scope_with_root(&temp_dir, "mixed-asset");

    let atomic_key = scope.test_key("meta/index.json");
    let streaming_key = scope.test_key("media/data.bin");

    let atomic_payload = b"{\"idx\":1}".to_vec();
    let streaming_payload = b"stream-bytes-123".to_vec();

    {
        let atomic = pending(scope.store().acquire_resource(&atomic_key, None).unwrap());
        atomic.write_at(0, &atomic_payload).unwrap();
        atomic.commit(Some(atomic_payload.len() as u64)).unwrap();

        let streaming = pending(
            scope
                .store()
                .acquire_resource(&streaming_key, None)
                .unwrap(),
        );
        streaming.write_at(0, &streaming_payload).unwrap();
        let streaming = streaming
            .commit(Some(streaming_payload.len() as u64))
            .unwrap();
        streaming
            .wait_range(0..streaming_payload.len() as u64)
            .unwrap();
    }

    let atomic = scope.store().open_resource(&atomic_key, None).unwrap();
    let mut atomic_read = BytePool::default().get();
    atomic.read_into(&mut atomic_read).unwrap();
    assert_eq!(&*atomic_read, &atomic_payload[..]);

    let streaming = scope.store().open_resource(&streaming_key, None).unwrap();
    streaming
        .wait_range(0..streaming_payload.len() as u64)
        .unwrap();
    let streaming_read = read_bytes(&streaming, 0, streaming_payload.len());
    assert_eq!(&streaming_read, &streaming_payload[..]);
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn streaming_resource_concurrent_write_and_read_across_handles(
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let scope = asset_scope_with_root(&temp_dir, "concurrent-asset");

    let key = scope.test_key("media/concurrent.bin");
    let payload: Vec<u8> = b"concurrent streaming data".to_vec();
    let payload_len = payload.len() as u64;
    let writer_res = pending(scope.store().acquire_resource(&key, None).unwrap());

    let scope_reader = scope.clone();
    let key_reader = key.clone();
    let payload_len_reader = payload_len;
    let reader = thread::spawn(move || {
        let res = scope_reader
            .store()
            .open_resource(&key_reader, None)
            .unwrap();
        res.wait_range(0..payload_len_reader).unwrap();
        let mut buf = BytePool::default().get_with(|b| b.resize(payload_len_reader as usize, 0));
        let n = res.read_at(0, &mut buf).unwrap();
        buf.truncate(n);
        buf.to_vec()
    });

    let payload_writer = payload.clone();
    let writer = thread::spawn(move || {
        writer_res.write_at(0, &payload_writer).unwrap();
        let reader = writer_res
            .commit(Some(payload_writer.len() as u64))
            .unwrap();
        reader.wait_range(0..payload_writer.len() as u64).unwrap();
    });

    let data = reader.join().unwrap();
    writer.join().unwrap();
    assert_eq!(&data, &payload);
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case("asset-hls-123", 3)]
#[case("asset-hls-456", 5)]
#[case("asset-hls-789", 2)]
fn hls_multi_file_streaming_and_atomic_roundtrip_with_pins_persisted(
    #[case] asset_root: &str,
    #[case] segment_count: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let dir = temp_dir.path().to_path_buf();
    let scope = asset_scope_with_root(&temp_dir, asset_root);

    let playlist_key = scope.test_key("master.m3u8");
    let playlist_bytes = b"#EXTM3U\n#EXT-X-VERSION:7\n".to_vec();

    let playlist = pending(scope.store().acquire_resource(&playlist_key, None).unwrap());
    playlist.write_at(0, &playlist_bytes).unwrap();
    let playlist = playlist.commit(Some(playlist_bytes.len() as u64)).unwrap();
    let mut playlist_read = BytePool::default().get();
    playlist.read_into(&mut playlist_read).unwrap();
    assert_eq!(&*playlist_read, &playlist_bytes[..]);

    let mut segments = Vec::new();
    for i in 0..segment_count.min(2) {
        let seg_key = scope.test_key(format!("segments/{:04}.m4s", i + 1));

        let seg = pending(scope.store().acquire_resource(&seg_key, None).unwrap());
        let seg_reader = seg.reader();
        segments.push((seg, seg_reader, i));
    }

    if let Some((seg1, seg1_reader, _)) = segments.first() {
        let a = vec![0xAAu8; 4096];
        let b = vec![0xBBu8; 2048];

        seg1.write_at(0, &a).unwrap();
        seg1.write_at(8192, &b).unwrap();

        seg1_reader.wait_range(0..(a.len() as u64)).unwrap();
        seg1_reader
            .wait_range(8192..(8192 + b.len() as u64))
            .unwrap();

        assert_eq!(read_bytes(seg1_reader, 0, a.len()), a);
        assert_eq!(read_bytes(seg1_reader, 8192, b.len()), b);
    }

    if let Some((seg2, seg2_reader, _)) = segments.get(1) {
        let c = vec![0xCCu8; 10 * 1024];
        seg2.write_at(0, &c).unwrap();
        seg2_reader.wait_range(0..(c.len() as u64)).unwrap();
        assert_eq!(read_bytes(seg2_reader, 0, c.len()), c);
    }

    let mut committed = Vec::new();
    for (seg, _seg_reader, _) in segments {
        committed.push(seg.commit(None).unwrap());
    }

    if let Some(pinned) = read_pins_file(&dir) {
        assert!(
            pinned.iter().any(|v| v == asset_root),
            "hls asset_root must be pinned while any resource is open if pins file exists"
        );
    }

    for seg in committed {
        drop(seg);
    }
    drop(playlist);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
#[case("asset-test-1", "media/file1.bin")]
#[case("asset-test-2", "deep/path/to/file2.bin")]
#[case("asset-test-3", "file3.bin")]
fn atomic_resource_roundtrip_with_different_paths(
    #[case] asset_root: &str,
    #[case] rel_path: &str,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let scope = asset_scope_with_root(&temp_dir, asset_root);

    let key = scope.test_key(rel_path);
    let payload = b"test data for atomic resource".to_vec();

    let writer = pending(scope.store().acquire_resource(&key, None).unwrap());

    writer.write_at(0, &payload).unwrap();
    let res = writer.commit(Some(payload.len() as u64)).unwrap();

    let mut read_back = BytePool::default().get();
    res.read_into(&mut read_back).unwrap();
    assert_eq!(&*read_back, &payload[..]);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
#[case(0, 4096, 4096)]
#[case(8192, 2048, 2048)]
#[case(16384, 10240, 10240)]
fn streaming_resource_write_read_at_different_positions(
    #[case] offset: u64,
    #[case] size: usize,
    #[case] read_size: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let scope = asset_scope_with_root(&temp_dir, "streaming-test");

    let key = scope.test_key("data.bin");
    let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
    let reader = writer.reader();

    let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

    writer.write_at(offset, &data).unwrap();
    reader.wait_range(offset..(offset + size as u64)).unwrap();

    let read_back = read_bytes(&reader, offset, read_size.min(size));
    assert_eq!(read_back, &data[..read_size.min(size)]);

    writer.commit(None).unwrap();
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
#[case(2)]
#[case(3)]
#[case(5)]
fn multiple_resources_same_asset_root_independently_accessible(
    #[case] resource_count: usize,
    temp_dir: kithara_integration_tests::TestTempDir,
) {
    let asset_root = "multi-resource-asset";
    let scope = asset_scope_with_root(&temp_dir, asset_root);

    let keys: Vec<ResourceKey> = (0..resource_count)
        .map(|i| {
            let rel_path = if i % 2 == 0 {
                format!("file{}.bin", i)
            } else {
                format!("subdir/file{}.bin", i)
            };
            scope.test_key(rel_path)
        })
        .collect();

    let mut resources = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let writer = pending(scope.store().acquire_resource(key, None).unwrap());

        let data = format!("data for file {}", i).into_bytes();
        writer.write_at(0, &data).unwrap();
        let res = writer.commit(Some(data.len() as u64)).unwrap();

        resources.push((res, data));
    }

    for (res, expected_data) in resources {
        let mut read_back = BytePool::default().get();
        res.read_into(&mut read_back).unwrap();
        assert_eq!(&*read_back, &expected_data[..]);
    }
}

/// Test that `delete_asset` only deletes the asset directory for the store's `asset_root`,
/// leaving other assets in the same `root_dir` untouched.
#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn delete_asset_only_removes_own_directory(temp_dir: kithara_integration_tests::TestTempDir) {
    let root_path = temp_dir.path();

    let asset_roots = ["asset-alpha", "asset-beta", "asset-gamma"];
    let payloads: [&[u8]; 3] = [b"alpha data", b"beta data", b"gamma data"];

    for (i, asset_root) in asset_roots.iter().enumerate() {
        let scope = asset_scope_with_root(&temp_dir, asset_root);
        let key = scope.test_key("data.bin");
        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        writer.write_at(0, payloads[i]).unwrap();
        writer.commit(Some(payloads[i].len() as u64)).unwrap();
    }

    for asset_root in &asset_roots {
        let asset_path = root_path.join(asset_root);
        assert!(
            asset_path.exists(),
            "asset directory {} should exist before deletion",
            asset_root
        );
    }

    {
        let scope = asset_scope_with_root(&temp_dir, "asset-beta");
        scope.delete_asset().unwrap();
    }

    assert!(
        !root_path.join("asset-beta").exists(),
        "asset-beta directory should be deleted"
    );

    assert!(
        root_path.join("asset-alpha").exists(),
        "asset-alpha directory should still exist"
    );
    {
        let scope = asset_scope_with_root(&temp_dir, "asset-alpha");
        let key = scope.test_key("data.bin");
        let res = scope.store().open_resource(&key, None).unwrap();
        let mut buf = BytePool::default().get();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, payloads[0], "asset-alpha data should be intact");
    }

    assert!(
        root_path.join("asset-gamma").exists(),
        "asset-gamma directory should still exist"
    );
    {
        let scope = asset_scope_with_root(&temp_dir, "asset-gamma");
        let key = scope.test_key("data.bin");
        let res = scope.store().open_resource(&key, None).unwrap();
        let mut buf = BytePool::default().get();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, payloads[2], "asset-gamma data should be intact");
    }
}

/// Test sequential deletion of multiple assets in the same `root_dir`.
#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn delete_assets_sequentially(temp_dir: kithara_integration_tests::TestTempDir) {
    let root_path = temp_dir.path();

    let asset_roots = ["seq-asset-1", "seq-asset-2", "seq-asset-3", "seq-asset-4"];

    for (i, asset_root) in asset_roots.iter().enumerate() {
        let scope = asset_scope_with_root(&temp_dir, asset_root);
        let key = scope.test_key(format!("file{}.bin", i));
        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        let content = format!("content {}", i).into_bytes();
        writer.write_at(0, &content).unwrap();
        writer.commit(Some(content.len() as u64)).unwrap();
    }

    for asset_root in &asset_roots {
        assert!(
            root_path.join(asset_root).exists(),
            "{} should exist",
            asset_root
        );
    }

    for (delete_idx, asset_to_delete) in asset_roots.iter().enumerate() {
        {
            let scope = asset_scope_with_root(&temp_dir, asset_to_delete);
            scope.delete_asset().unwrap();
        }

        assert!(
            !root_path.join(asset_to_delete).exists(),
            "{} should be deleted",
            asset_to_delete
        );

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

    for asset_root in &asset_roots {
        assert!(
            !root_path.join(asset_root).exists(),
            "{} should not exist after sequential deletion",
            asset_root
        );
    }
}

/// Test that deleting a non-existent asset doesn't affect other assets.
#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn delete_nonexistent_asset_is_idempotent(temp_dir: kithara_integration_tests::TestTempDir) {
    let root_path = temp_dir.path();

    {
        let scope = asset_scope_with_root(&temp_dir, "existing-asset");
        let key = scope.test_key("data.bin");
        let writer = pending(scope.store().acquire_resource(&key, None).unwrap());
        let data: &[u8] = b"existing data";
        writer.write_at(0, data).unwrap();
        writer.commit(Some(data.len() as u64)).unwrap();
    }

    {
        let scope = asset_scope_with_root(&temp_dir, "nonexistent-asset");
        let result = scope.delete_asset();
        assert!(result.is_ok(), "deleting non-existent asset should succeed");
    }

    assert!(
        root_path.join("existing-asset").exists(),
        "existing-asset should still exist"
    );
    {
        let scope = asset_scope_with_root(&temp_dir, "existing-asset");
        let key = scope.test_key("data.bin");
        let res = scope.store().open_resource(&key, None).unwrap();
        let mut buf = BytePool::default().get();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, b"existing data");
    }
}
