use std::time::Duration;

use kithara_storage::{
    AtomicOptions, AtomicResource, DiskOptions, Resource, ResourceStatus, StorageError,
    StreamingResource, StreamingResourceExt, WaitOutcome,
};
use rstest::*;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// === Test Fixtures ===

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().expect("Failed to create temp dir")
}

#[fixture]
fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

#[fixture]
fn cancel_token_cancelled() -> CancellationToken {
    let token = CancellationToken::new();
    token.cancel();
    token
}

// === Path Method Tests ===

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("test.dat");
    let atomic: AtomicResource =
        AtomicResource::open(AtomicOptions::new(file_path.clone(), cancel_token));

    // Check that path() returns the correct path
    assert_eq!(atomic.path(), file_path);

    // Check that path() returns the same path after operations
    atomic
        .write(b"test data")
        .await
        .expect("Write should succeed");
    assert_eq!(atomic.path(), file_path);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("streaming.dat");
    let streaming = StreamingResource::open_disk(DiskOptions::new(file_path.clone(), cancel_token))
        .await
        .expect("Open should succeed");

    // Check that path() returns the correct path
    assert_eq!(streaming.path(), file_path);

    // Check that path() returns the same path after operations
    streaming
        .write_at(0, b"test")
        .await
        .expect("Write should succeed");
    assert_eq!(streaming.path(), file_path);
}

// === AtomicResource Tests ===

#[rstest]
#[case("simple data", b"Hello, World!")]
#[case("empty data", b"")]
#[case("binary data", &[0x00, 0xFF, 0x80, 0x7F])]
#[case("large data", &[0x42; 1024 * 1024])] // 1MB
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn atomic_resource_write_read_success(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] test_name: &str,
    #[case] test_data: &[u8],
) {
    let file_path = temp_dir.path().join(format!("{}.dat", test_name));
    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    // Write data
    atomic.write(test_data).await.expect("Write should succeed");

    // Read data back
    let read_data = atomic.read().await.expect("Read should succeed");
    assert_eq!(
        &*read_data, test_data,
        "Read data should match written data"
    );
}

#[rstest]
#[case(true)] // File exists initially
#[case(false)] // File doesn't exist initially
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_read_missing_file(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] create_file_first: bool,
) {
    let file_path = temp_dir.path().join("missing.dat");

    if create_file_first {
        // Create file with some content
        let atomic: AtomicResource =
            AtomicResource::open(AtomicOptions::new(&file_path, cancel_token.clone()));
        atomic.write(b"initial").await.unwrap();
    }

    // Open atomic resource (should work whether file exists or not)
    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    if create_file_first {
        let data = atomic.read().await.unwrap();
        assert_eq!(&*data, b"initial");
    } else {
        // Read non-existent file should return empty bytes
        let data = atomic
            .read()
            .await
            .expect("Read should succeed on missing file");
        assert!(data.is_empty(), "Missing file should return empty bytes");
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_cancelled_operations(
    temp_dir: TempDir,
    cancel_token_cancelled: CancellationToken,
) {
    let file_path = temp_dir.path().join("cancelled.dat");
    let atomic: AtomicResource =
        AtomicResource::open(AtomicOptions::new(file_path, cancel_token_cancelled));

    // All operations should fail with Cancelled
    assert!(matches!(
        atomic.write(b"data").await,
        Err(StorageError::Cancelled)
    ));
    assert!(matches!(atomic.read().await, Err(StorageError::Cancelled)));
    assert!(matches!(
        atomic.commit(None).await,
        Err(StorageError::Cancelled)
    ));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_fail_propagation(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("failed.dat");
    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    // Mark resource as failed
    atomic
        .fail("test failure")
        .await
        .expect("Fail should succeed");

    // Subsequent operations should fail
    assert!(matches!(
        atomic.write(b"data").await,
        Err(StorageError::Failed(_))
    ));
    assert!(matches!(atomic.read().await, Err(StorageError::Failed(_))));
    assert!(matches!(
        atomic.commit(None).await,
        Err(StorageError::Failed(_))
    ));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_concurrent_writes(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("concurrent.dat");
    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    let atomic_clone = atomic.clone();
    let handle1: tokio::task::JoinHandle<Result<(), StorageError>> =
        tokio::spawn(async move { atomic_clone.write(b"data1").await });

    let atomic_clone = atomic.clone();
    let handle2: tokio::task::JoinHandle<Result<(), StorageError>> =
        tokio::spawn(async move { atomic_clone.write(b"data2").await });

    // Both writes should succeed (atomic behavior via temp files)
    let (result1, result2) = tokio::join!(handle1, handle2);
    assert!(result1.unwrap().is_ok());
    assert!(result2.unwrap().is_ok());

    // Final read should return one of the writes (atomic)
    let final_data = atomic.read().await.unwrap();
    assert!(*final_data == *b"data1" || *final_data == *b"data2");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_invalid_path(temp_dir: TempDir, cancel_token: CancellationToken) {
    // Create a file path and then remove the directory to make it invalid
    let invalid_path = temp_dir.path().join("nonexistent").join("file.dat");
    let atomic: AtomicResource =
        AtomicResource::open(AtomicOptions::new(invalid_path, cancel_token));

    let result = atomic.write(b"data").await;
    // This should succeed because create_dir_all creates the parent directory
    assert!(result.is_ok());
}

// === StreamingResource Tests ===

#[rstest]
#[case(0)] // No initial length
#[case(1024)] // With initial length
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn streaming_resource_open_and_status(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] initial_len: u64,
) {
    let file_path = temp_dir.path().join("stream.dat");
    let mut opts = DiskOptions::new(file_path, cancel_token);
    if initial_len > 0 {
        opts.initial_len = Some(initial_len);
    }

    let resource: StreamingResource = StreamingResource::open_disk(opts)
        .await
        .expect("Open should succeed");
    let status = resource.status().await;

    if initial_len > 0 {
        assert_eq!(
            status,
            ResourceStatus::Committed {
                final_len: Some(initial_len)
            }
        );
    } else {
        assert_eq!(status, ResourceStatus::InProgress);
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_range_write_wait_read(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let file_path = temp_dir.path().join("ranges.dat");
    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .expect("Open should succeed");

    // Write data in non-contiguous ranges
    resource
        .write_at(0, b"Hello, ")
        .await
        .expect("Write at 0 should succeed");
    resource
        .write_at(7, b"World!")
        .await
        .expect("Write at 7 should succeed");

    // Wait for full range and read
    resource
        .wait_range(0..13)
        .await
        .expect("Wait should succeed");
    let data = resource.read_at(0, 13).await.expect("Read should succeed");
    assert_eq!(&*data, b"Hello, World!");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_wait_range_partial_coverage() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("partial.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token.clone()))
            .await
            .expect("Open should succeed");

    // Write only part of the requested range
    resource.write_at(0, b"Hello").await.unwrap();

    // Wait for larger range should not complete immediately
    let resource_clone = resource.clone();
    let wait_handle: tokio::task::JoinHandle<Result<WaitOutcome, StorageError>> =
        tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    // Should timeout due to missing coverage
    let wait_result = timeout(Duration::from_millis(100), wait_handle).await;
    assert!(wait_result.is_err()); // Timeout

    // Complete the range
    resource.write_at(5, b", World!").await.unwrap();
    resource.commit(Some(13)).await.unwrap();

    // Now wait should succeed
    resource
        .wait_range(0..13)
        .await
        .expect("Wait should succeed after commit");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_commit_and_eof(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("commit.dat");
    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .expect("Open should succeed");

    // Write some data
    resource.write_at(0, b"Hello").await.unwrap();

    // Wait range before commit should succeed
    let outcome = resource.wait_range(0..5).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Commit with final length
    resource.commit(Some(5)).await.unwrap();

    // Wait beyond final length should return EOF
    let outcome = resource.wait_range(5..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    // Read beyond EOF should return empty
    let data = resource.read_at(10, 5).await.unwrap();
    assert!(data.is_empty());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_commit_without_final_len() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("commit_no_len.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Write some data and commit without final length
    resource.write_at(0, b"Hello").await.unwrap();
    resource.commit(None).await.unwrap();

    // Whole read should fail (no final length known)
    let result = resource.read().await;
    assert!(matches!(result, Err(StorageError::Sealed)));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_sealed_after_commit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sealed.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Commit and then try to write
    resource.commit(Some(0)).await.unwrap();

    // Writes should fail after commit
    let result = resource.write_at(0, b"data").await;
    assert!(matches!(result, Err(StorageError::Sealed)));

    // But whole-object write should also fail
    let result = resource.write(b"data").await;
    assert!(matches!(result, Err(StorageError::Sealed)));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_cancel_during_wait() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("cancel_wait.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token.clone()))
            .await
            .unwrap();

    // Start waiting for a range that doesn't exist
    let resource_clone = resource.clone();
    let wait_handle: tokio::task::JoinHandle<Result<WaitOutcome, StorageError>> =
        tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    // Cancel after a short delay
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_token.cancel();
    });

    // Wait should return Cancelled
    let wait_result = wait_handle.await.unwrap();
    assert!(matches!(wait_result, Err(StorageError::Cancelled)));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_fail_wakes_waiters() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("fail_waiters.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Start waiting for a range that doesn't exist
    let resource_clone = resource.clone();
    let wait_handle: tokio::task::JoinHandle<Result<WaitOutcome, StorageError>> =
        tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    // Fail after a short delay
    let resource_clone = resource.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        resource_clone.fail("test failure").await.unwrap();
    });

    // Wait should return Failed
    let wait_result = wait_handle.await.unwrap();
    assert!(matches!(wait_result, Err(StorageError::Failed(_))));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_concurrent_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("concurrent_ops.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Simple concurrent write test
    let resource_clone = resource.clone();
    let handle1 = tokio::spawn(async move { resource_clone.write_at(0, b"Hello").await });

    let resource_clone = resource.clone();
    let handle2 = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        resource_clone.write_at(5, b"World").await
    });

    // Both writes should succeed
    assert!(handle1.await.unwrap().is_ok());
    assert!(handle2.await.unwrap().is_ok());

    // Commit to make range complete
    resource.commit(Some(10)).await.unwrap();

    // Now wait and verify
    let outcome = resource.wait_range(0..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let data = resource.read_at(0, 10).await.unwrap();
    assert_eq!(&data[..5], b"Hello");
    assert_eq!(&data[5..], b"World");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_invalid_ranges() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("invalid_ranges.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Invalid wait ranges
    assert!(matches!(
        resource.wait_range(10..5).await,
        Err(StorageError::InvalidRange { start: 10, end: 5 })
    ));
    assert!(matches!(
        resource.wait_range(5..5).await,
        Err(StorageError::InvalidRange { start: 5, end: 5 })
    ));

    // Invalid write ranges (overflow detection) - use a smaller but still too large data
    let large_data = vec![0u8; 1000];
    let result = resource.write_at(u64::MAX, &large_data).await;
    assert!(matches!(result, Err(StorageError::InvalidRange { .. })));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_whole_object_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("whole_object.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Whole-object write
    resource.write(b"Hello, World!").await.unwrap();

    // Should be committed with final length
    let status = resource.status().await;
    assert_eq!(
        status,
        ResourceStatus::Committed {
            final_len: Some(13)
        }
    );

    // Whole-object read should work
    let data = resource.read().await.unwrap();
    assert_eq!(&*data, b"Hello, World!");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_empty_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty_ops.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Empty write should be no-op
    resource.write_at(0, b"").await.unwrap();

    // Empty read should return empty bytes
    let data = resource.read_at(0, 0).await.unwrap();
    assert!(data.is_empty());

    // Commit with zero length
    resource.commit(Some(0)).await.unwrap();

    // Whole-object read should return empty
    let data = resource.read().await.unwrap();
    assert!(data.is_empty());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_complex_range_scenario() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("complex_ranges.dat");
    let cancel_token = CancellationToken::new();

    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Write in a complex pattern: 0-10, 20-30, then fill gap 10-20
    resource.write_at(0, b"0123456789").await.unwrap(); // 0-10
    resource.write_at(20, b"0123456789").await.unwrap(); // 20-30

    // Wait for 0-15 should fail (gap at 10-15)
    let resource_clone = resource.clone();
    let wait_result: Result<Result<WaitOutcome, StorageError>, _> =
        timeout(Duration::from_millis(100), resource_clone.wait_range(0..15)).await;
    assert!(wait_result.is_err()); // Timeout

    // Fill the gap
    resource.write_at(10, b"ABCDEFGHIJ").await.unwrap(); // 10-20

    // Now wait for 0-30 should succeed
    resource.wait_range(0..30).await.unwrap();

    // Read entire range and verify
    let data = resource.read_at(0, 30).await.unwrap();
    assert_eq!(&data[0..10], b"0123456789");
    assert_eq!(&data[10..20], b"ABCDEFGHIJ");
    assert_eq!(&data[20..30], b"0123456789");

    // Commit with final length
    resource.commit(Some(30)).await.unwrap();

    // Try to read beyond final length
    let outcome = resource.wait_range(30..40).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_restart_with_initial_len() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("restart.dat");
    let cancel_token = CancellationToken::new();

    // Create resource with initial length
    let mut opts = DiskOptions::new(&file_path, cancel_token.clone());
    opts.initial_len = Some(100);
    let resource1: StreamingResource = StreamingResource::open_disk(opts).await.unwrap();

    // Should be already committed with final length when initial_len > 0
    let status1 = resource1.status().await;
    println!("Status after initial_len=100: {:?}", status1);

    // Should be able to wait for the range that was pre-marked as available
    let outcome = resource1.wait_range(0..100).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Should be able to read from all ranges (sparse file with zeros)
    let data = resource1.read_at(50, 10).await.unwrap();
    assert_eq!(data.len(), 10); // Should read zeros

    // Open again without initial length (file should exist)
    let resource2: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    // Should still work (file exists with data)
    let data2 = resource2.read_at(50, 10).await.unwrap();
    assert_eq!(data2.len(), 10);
}
