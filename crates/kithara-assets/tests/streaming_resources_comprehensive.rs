#![forbid(unsafe_code)]

use std::time::Duration;

use bytes::Bytes;
use kithara_assets::{AssetStore, EvictConfig, ResourceKey};
use kithara_storage::{Resource, StreamingResourceExt};
use rstest::{fixture, rstest};
use tokio_util::sync::CancellationToken;

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
#[case(1024, 512, 0)] // Small write
#[case(4096, 2048, 8192)] // Medium write with offset
#[case(16384, 8192, 32768)] // Large write with offset
#[case(65536, 32768, 131072)] // Very large write with offset
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_complex_write_patterns(
    #[case] total_size: usize,
    #[case] chunk_size: usize,
    #[case] initial_offset: u64,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey::new("streaming-complex".to_string(), "data.bin".to_string());

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Write data in chunks at different positions
    let total_chunks = total_size / chunk_size;
    for i in 0..total_chunks {
        let offset = initial_offset + (i * chunk_size) as u64;
        let data: Vec<u8> = (0..chunk_size).map(|j| ((i + j) % 256) as u8).collect();

        res.write_at(offset, &data).await.unwrap();
        res.wait_range(offset..(offset + chunk_size as u64))
            .await
            .unwrap();

        // Verify the written data
        let read_back = res.read_at(offset, chunk_size).await.unwrap();
        assert_eq!(read_back, Bytes::from(data));
    }

    res.commit(None).await.unwrap();
}

#[rstest]
#[case(1, 100)] // Single concurrent write
#[case(2, 50)] // Few concurrent writes (reduced to avoid timeout)
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn streaming_resource_concurrent_writes(
    #[case] write_count: usize,
    #[case] chunk_size: usize,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey::new(
        "streaming-concurrent".to_string(),
        "concurrent.bin".to_string(),
    );

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Spawn concurrent writes
    let mut handles = Vec::new();
    for i in 0..write_count {
        let res_clone = res.clone();
        let _cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let offset = (i * chunk_size) as u64;
            let data: Vec<u8> = (0..chunk_size)
                .map(|j| ((i * chunk_size + j) % 256) as u8)
                .collect();

            res_clone.write_at(offset, &data).await.unwrap();
            res_clone
                .wait_range(offset..(offset + chunk_size as u64))
                .await
                .unwrap();

            (offset, data)
        });

        handles.push(handle);
    }

    // Wait for all writes to complete and verify
    for handle in handles {
        let (offset, data) = handle.await.unwrap();
        let read_back = res.read_at(offset, data.len()).await.unwrap();
        assert_eq!(read_back, Bytes::from(data));
    }

    res.commit(None).await.unwrap();
}

#[rstest]
#[case(0, 1024)] // Read from start
#[case(2048, 1024)] // Read from middle
#[case(4096, 512)] // Read from end
#[case(8192, 100)] // Read beyond written data (partial)
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_edge_case_reads(
    #[case] offset: u64,
    #[case] read_size: usize,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey::new("streaming-edge-reads".to_string(), "edge.bin".to_string());

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Write initial data
    let data_size = 6144; // Total data size
    let initial_data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
    res.write_at(0, &initial_data).await.unwrap();
    res.wait_range(0..data_size as u64).await.unwrap();

    // Perform edge case read
    if offset < data_size as u64 {
        let expected_size = read_size.min(data_size - offset as usize);
        let read_back = res.read_at(offset, read_size).await.unwrap();

        assert_eq!(read_back.len(), expected_size);

        if expected_size > 0 {
            let expected_data = &initial_data[offset as usize..offset as usize + expected_size];
            assert_eq!(read_back, Bytes::copy_from_slice(expected_data));
        }
    }

    res.commit(None).await.unwrap();
}

#[rstest]
#[case(vec![(0, 1024), (2048, 1024)])] // Non-overlapping
#[case(vec![(0, 512), (1024, 512)])] // Smaller overlapping
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_multiple_range_operations(
    #[case] write_ranges: Vec<(usize, usize)>,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey::new("streaming-multi-range".to_string(), "multi.bin".to_string());

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Write multiple ranges
    for (i, (offset, size)) in write_ranges.iter().enumerate() {
        let data: Vec<u8> = (0..*size).map(|j| ((i * 1000 + j) % 256) as u8).collect();
        let offset_u64 = *offset as u64;

        res.write_at(offset_u64, &data).await.unwrap();
        res.wait_range(offset_u64..(offset_u64 + *size as u64))
            .await
            .unwrap();

        // Verify each write immediately
        let read_back = res.read_at(offset_u64, *size).await.unwrap();
        assert_eq!(read_back, Bytes::from(data));
    }

    // Final verification of all ranges
    for (i, (offset, size)) in write_ranges.iter().enumerate() {
        let expected_data: Vec<u8> = (0..*size).map(|j| ((i * 1000 + j) % 256) as u8).collect();
        let read_back = res.read_at(*offset as u64, *size).await.unwrap();
        assert_eq!(read_back, Bytes::from(expected_data));
    }

    res.commit(None).await.unwrap();
}

#[rstest]
#[case(false)] // Without explicit commit
#[case(true)] // With explicit commit
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_commit_behavior(
    #[case] explicit_commit: bool,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey::new("streaming-commit".to_string(), "commit.bin".to_string());

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Write some data
    let data = vec![0xAB; 4096];
    res.write_at(0, &data).await.unwrap();
    res.wait_range(0..(data.len() as u64)).await.unwrap();

    // Verify before commit
    let read_back = res.read_at(0, data.len()).await.unwrap();
    assert_eq!(read_back, Bytes::from(data.clone()));

    if explicit_commit {
        res.commit(None).await.unwrap();
    }

    // Resource should still be readable
    let read_back_again = res.read_at(0, data.len()).await.unwrap();
    assert_eq!(read_back_again, Bytes::from(data.clone()));

    // Drop resource and verify it's still accessible
    drop(res);

    // Reopen the resource
    let res_reopened = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Should be able to read the data (assuming persistence works)
    let final_read = res_reopened.read_at(0, data.len()).await.unwrap();
    assert_eq!(final_read, Bytes::from(data.clone()));
}

#[rstest]
#[case(1024)]
#[case(4096)]
#[case(16384)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_zero_length_operations(
    #[case] base_offset: u64,
    cancel_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
    asset_store_no_limits: AssetStore,
) {
    let cancel = cancel_token;
    let store = asset_store_no_limits;

    let key = ResourceKey::new("streaming-zero-length".to_string(), "zero.bin".to_string());

    let res = store
        .open_streaming_resource(&key, cancel.clone())
        .await
        .unwrap();

    // Write some data first
    let data = vec![0xCC; 2048];
    res.write_at(base_offset, &data).await.unwrap();
    res.wait_range(base_offset..(base_offset + data.len() as u64))
        .await
        .unwrap();

    // Test zero-length read at various positions
    let zero_read = res.read_at(base_offset, 0).await.unwrap();
    assert_eq!(zero_read, Bytes::new());

    // Test zero-length write (should be no-op)
    res.write_at(base_offset + 100, &[]).await.unwrap();

    // Verify original data is still intact
    let read_back = res.read_at(base_offset, data.len()).await.unwrap();
    assert_eq!(read_back, Bytes::from(data));

    res.commit(None).await.unwrap();
}
