// StreamingResource tests (merged from edge cases)
use std::time::Duration;

use kithara_storage::{
    DiskOptions, Resource, ResourceStatus, StorageError, StreamingResource, StreamingResourceExt,
    WaitOutcome,
};
use rstest::*;
use tempfile::TempDir;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

/// Helper to read bytes from resource into a new Vec
async fn read_bytes<R: StreamingResourceExt>(res: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let n = res.read_at(offset, &mut buf).await.unwrap_or(0);
    buf.truncate(n);
    buf
}

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().expect("failed to create temp dir")
}

#[fixture]
fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("streaming.dat");
    let streaming = StreamingResource::open_disk(DiskOptions::new(file_path.clone(), cancel_token))
        .await
        .expect("open should succeed");

    assert_eq!(streaming.path(), file_path);

    streaming
        .write_at(0, b"test")
        .await
        .expect("write should succeed");
    assert_eq!(streaming.path(), file_path);
}

#[rstest]
#[case(0)]
#[case(1024)]
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

    let resource: StreamingResource = StreamingResource::open_disk(opts).await.unwrap();
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
            .unwrap();

    resource.write_at(0, b"Hello, ").await.unwrap();
    resource.write_at(7, b"World!").await.unwrap();

    resource.wait_range(0..13).await.unwrap();
    let data = read_bytes(&resource, 0, 13).await;
    assert_eq!(&data, b"Hello, World!");
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

    resource.write_at(100, b"start").await.unwrap();
    resource.write_at(1000, b"middle").await.unwrap();
    resource.write_at(10000, b"end").await.unwrap();

    resource.wait_range(100..105).await.unwrap();
    resource.wait_range(1000..1006).await.unwrap();
    resource.wait_range(10000..10003).await.unwrap();

    assert_eq!(&*read_bytes(&resource, 100, 5).await, b"start");
    assert_eq!(&*read_bytes(&resource, 1000, 6).await, b"middle");
    assert_eq!(&*read_bytes(&resource, 10000, 3).await, b"end");
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

    resource.write_at(0, b"Hello World!").await.unwrap();
    resource.write_at(6, b"Kithara!").await.unwrap();

    resource.wait_range(0..14).await.unwrap();
    let data = read_bytes(&resource, 0, 14).await;
    assert_eq!(&data, b"Hello Kithara!");
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

    resource.commit(Some(0)).await.unwrap();

    let status = resource.status().await;
    assert_eq!(status, ResourceStatus::Committed { final_len: Some(0) });

    let outcome = resource.wait_range(0..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    let data = read_bytes(&resource, 0, 0).await;
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

    resource.write_at(0, b"X").await.unwrap();

    resource.wait_range(0..1).await.unwrap();
    let data = read_bytes(&resource, 0, 1).await;
    assert_eq!(&data, b"X");

    let data = read_bytes(&resource, 0, 0).await;
    assert!(data.is_empty());

    resource.commit(Some(1)).await.unwrap();
    let data = read_bytes(&resource, 0, 10).await;
    assert_eq!(&data, b"X");
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

    let resource_clone = resource.clone();
    let wait_handle = tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    sleep(Duration::from_millis(10)).await;

    resource.write_at(0, b"0123456789").await.unwrap();

    let wait_result = wait_handle.await.unwrap().unwrap();
    assert_eq!(wait_result, WaitOutcome::Ready);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_persists_across_reopen() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("reopen.dat");
    let cancel_token = CancellationToken::new();

    {
        let resource: StreamingResource =
            StreamingResource::open_disk(DiskOptions::new(file_path.clone(), cancel_token.clone()))
                .await
                .unwrap();

        resource.write_at(0, b"persisted data").await.unwrap();
        resource.commit(Some(14)).await.unwrap();
        resource.wait_range(0..14).await.unwrap();

        let data = read_bytes(&resource, 0, 14).await;
        assert_eq!(&data, b"persisted data");
    }

    let file_len = tokio::fs::metadata(&file_path).await.unwrap().len();
    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, CancellationToken::new()))
            .await
            .unwrap();

    let data = read_bytes(&resource, 0, file_len as usize).await;
    assert_eq!(&data, b"persisted data");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_wait_after_reopen() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("wait_reopen.dat");
    let cancel_token = CancellationToken::new();
    let payload = b"waited bytes";

    {
        let resource: StreamingResource =
            StreamingResource::open_disk(DiskOptions::new(file_path.clone(), cancel_token.clone()))
                .await
                .unwrap();

        resource.write_at(0, payload).await.unwrap();
        resource.commit(Some(payload.len() as u64)).await.unwrap();
        resource.wait_range(0..payload.len() as u64).await.unwrap();
    }

    let file_len = tokio::fs::metadata(&file_path).await.unwrap().len();
    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, CancellationToken::new()))
            .await
            .unwrap();

    let outcome = resource.wait_range(0..file_len).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let data = read_bytes(&resource, 0, file_len as usize).await;
    assert_eq!(&data, payload);
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
            .unwrap();

    resource.write_at(0, b"Hello").await.unwrap();

    let resource_clone = resource.clone();
    let wait_handle = tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    let wait_result = timeout(Duration::from_millis(100), wait_handle).await;
    assert!(wait_result.is_err());

    resource.write_at(5, b", World!").await.unwrap();
    resource.commit(Some(13)).await.unwrap();

    resource.wait_range(0..13).await.unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn streaming_resource_commit_and_eof(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("commit.dat");
    let resource: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    resource.write_at(0, b"Hello").await.unwrap();

    let outcome = resource.wait_range(0..5).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    resource.commit(Some(5)).await.unwrap();

    let outcome = resource.wait_range(5..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    let data = read_bytes(&resource, 10, 5).await;
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

    resource.write_at(0, b"Hello").await.unwrap();
    resource.commit(None).await.unwrap();

    // Commit without final_len means we don't know the total size.
    // read_at still works, but wait_range doesn't know when EOF is reached.
    let status = resource.status().await;
    assert_eq!(status, ResourceStatus::Committed { final_len: None });

    // We can still read the written data
    let data = read_bytes(&resource, 0, 5).await;
    assert_eq!(&data, b"Hello");
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

    resource.commit(Some(0)).await.unwrap();

    resource.write_at(0, b"data").await.unwrap();
    resource.commit(Some(4)).await.unwrap();

    let data = read_bytes(&resource, 0, 4).await;
    assert_eq!(&data, b"data");
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

    let resource_clone = resource.clone();
    let wait_handle = tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        cancel_token.cancel();
    });

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

    let resource_clone = resource.clone();
    let wait_handle = tokio::spawn(async move { resource_clone.wait_range(0..10).await });

    let resource_clone = resource.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        resource_clone.fail("test failure").await.unwrap();
    });

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

    let resource_clone = resource.clone();
    let handle1 = tokio::spawn(async move { resource_clone.write_at(0, b"Hello").await });

    let resource_clone = resource.clone();
    let handle2 = tokio::spawn(async move {
        sleep(Duration::from_millis(10)).await;
        resource_clone.write_at(5, b"World").await
    });

    assert!(handle1.await.unwrap().is_ok());
    assert!(handle2.await.unwrap().is_ok());

    resource.commit(Some(10)).await.unwrap();

    let outcome = resource.wait_range(0..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let data = read_bytes(&resource, 0, 10).await;
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

    assert!(matches!(
        resource
            .wait_range(std::ops::Range { start: 10, end: 5 })
            .await,
        Err(StorageError::InvalidRange { start: 10, end: 5 })
    ));
    assert!(matches!(
        resource.wait_range(5..5).await,
        Err(StorageError::InvalidRange { start: 5, end: 5 })
    ));

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

    resource.write_at(0, b"Hello, World!").await.unwrap();
    resource.commit(Some(13)).await.unwrap();

    let status = resource.status().await;
    assert_eq!(
        status,
        ResourceStatus::Committed {
            final_len: Some(13)
        }
    );

    resource.wait_range(0..13).await.unwrap();
    let data = read_bytes(&resource, 0, 13).await;
    assert_eq!(&data, b"Hello, World!");
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

    resource.write_at(0, b"").await.unwrap();

    let data = read_bytes(&resource, 0, 0).await;
    assert!(data.is_empty());

    resource.commit(Some(0)).await.unwrap();

    let data = read_bytes(&resource, 0, 0).await;
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

    resource.write_at(0, b"0123456789").await.unwrap();
    resource.write_at(20, b"0123456789").await.unwrap();

    let resource_clone = resource.clone();
    let wait_result: Result<Result<WaitOutcome, StorageError>, _> =
        timeout(Duration::from_millis(100), resource_clone.wait_range(0..15)).await;
    assert!(wait_result.is_err());

    resource.write_at(10, b"ABCDEFGHIJ").await.unwrap();

    resource.wait_range(0..30).await.unwrap();

    let data = read_bytes(&resource, 0, 30).await;
    assert_eq!(&data[0..10], b"0123456789");
    assert_eq!(&data[10..20], b"ABCDEFGHIJ");
    assert_eq!(&data[20..30], b"0123456789");

    resource.commit(Some(30)).await.unwrap();

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

    let mut opts = DiskOptions::new(&file_path, cancel_token.clone());
    opts.initial_len = Some(100);
    let resource1: StreamingResource = StreamingResource::open_disk(opts).await.unwrap();

    let outcome = resource1.wait_range(0..100).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let data = read_bytes(&resource1, 50, 10).await;
    assert_eq!(data.len(), 10);

    let resource2: StreamingResource =
        StreamingResource::open_disk(DiskOptions::new(file_path, cancel_token))
            .await
            .unwrap();

    let data2 = read_bytes(&resource2, 50, 10).await;
    assert_eq!(data2.len(), 10);
}
