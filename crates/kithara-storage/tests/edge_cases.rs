use std::time::Duration;

use kithara_storage::{
    AtomicOptions, AtomicResource, DiskOptions, Resource, ResourceStatus, StreamingResource,
    StreamingResourceExt, WaitOutcome,
};
use rstest::*;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_large_file_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("large.dat");
    let cancel_token = CancellationToken::new();

    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    // Write 10MB of data
    let large_data = vec![0x42; 10 * 1024 * 1024];
    atomic.write(&large_data).await.unwrap();

    // Read it back and verify
    let read_data = atomic.read().await.unwrap();
    assert_eq!(read_data.len(), large_data.len());
    assert_eq!(&*read_data, large_data.as_slice());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_sparse_file_behavior() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sparse.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Write data at non-contiguous positions, leaving gaps
    resource.write_at(100, b"start").await.unwrap();
    resource.write_at(1000, b"middle").await.unwrap();
    resource.write_at(10000, b"end").await.unwrap();

    // Wait for each individual range
    resource.wait_range(100..105).await.unwrap();
    resource.wait_range(1000..1006).await.unwrap();
    resource.wait_range(10000..10003).await.unwrap();

    // Verify each range independently
    let data1 = resource.read_at(100, 5).await.unwrap();
    assert_eq!(&*data1, b"start");

    let data2 = resource.read_at(1000, 6).await.unwrap();
    assert_eq!(&*data2, b"middle");

    let data3 = resource.read_at(10000, 3).await.unwrap();
    assert_eq!(&*data3, b"end");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_overlapping_writes() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("overlap.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // First write
    resource.write_at(0, b"Hello World!").await.unwrap();

    // Overlapping write (same length as "World")
    resource.write_at(6, b"Kithara!").await.unwrap();

    // Wait and verify the result
    resource.wait_range(0..14).await.unwrap();
    let data = resource.read_at(0, 14).await.unwrap();
    assert_eq!(&*data, b"Hello Kithara!");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_zero_length_commit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("zero.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Commit with zero length
    resource.commit(Some(0)).await.unwrap();

    // Should be in committed state
    let status = resource.status().await;
    assert_eq!(status, ResourceStatus::Committed { final_len: Some(0) });

    // All wait operations should return EOF
    let outcome = resource.wait_range(0..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    // Reads should return empty
    let data = resource.read().await.unwrap();
    assert!(data.is_empty());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_edge_case_ranges() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("edges.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Write exactly one byte
    resource.write_at(0, b"X").await.unwrap();

    // Test edge cases
    resource.wait_range(0..1).await.unwrap();

    // Read with exact length
    let data = resource.read_at(0, 1).await.unwrap();
    assert_eq!(&*data, b"X");

    // Read with zero length
    let data = resource.read_at(0, 0).await.unwrap();
    assert!(data.is_empty());

    // Read from position 0 with more than available
    resource.commit(Some(1)).await.unwrap();
    let data = resource.read_at(0, 10).await.unwrap();
    assert_eq!(&*data, b"X");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_concurrent_wait_and_write() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("concurrent_wait.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Start a waiter
    let resource_clone = resource.clone();
    let wait_handle = tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    // Give the waiter a moment to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Write the data
    resource.write_at(0, b"0123456789").await.unwrap();

    // Wait should complete successfully
    let wait_result = wait_handle.await.unwrap().unwrap();
    assert_eq!(wait_result, WaitOutcome::Ready);
}
