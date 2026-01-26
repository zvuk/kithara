//! Integration tests for StreamingResource.

use kithara_storage::{DiskOptions, Resource, StreamingResource, StreamingResourceExt, WaitOutcome};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_write_batched_basic() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("test_batched.bin");
    let cancel = CancellationToken::new();

    let opts = DiskOptions::new(&path, cancel);
    let resource = StreamingResource::open_disk(opts).await.unwrap();

    // Create 256KB of test data (should be split into 4 × 64KB batches)
    let data: Vec<u8> = (0..256 * 1024).map(|i| (i % 256) as u8).collect();

    // Write using batched method
    resource.write_batched(0, &data).await.unwrap();
    resource.commit(Some(data.len() as u64)).await.unwrap();

    // Verify data was written correctly
    let outcome = resource.wait_range(0..data.len() as u64).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let mut read_buf = vec![0u8; data.len()];
    let bytes_read = resource.read_at(0, &mut read_buf).await.unwrap();
    assert_eq!(bytes_read, data.len());
    assert_eq!(read_buf, data);
}

#[tokio::test]
async fn test_write_batched_small_data() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("test_batched_small.bin");
    let cancel = CancellationToken::new();

    let opts = DiskOptions::new(&path, cancel);
    let resource = StreamingResource::open_disk(opts).await.unwrap();

    // Small data (less than one batch)
    let data = b"hello world";

    resource.write_batched(0, data).await.unwrap();
    resource.commit(Some(data.len() as u64)).await.unwrap();

    let mut read_buf = vec![0u8; data.len()];
    let bytes_read = resource.read_at(0, &mut read_buf).await.unwrap();
    assert_eq!(bytes_read, data.len());
    assert_eq!(&read_buf, data);
}

#[tokio::test]
async fn test_write_batched_empty_data() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("test_batched_empty.bin");
    let cancel = CancellationToken::new();

    let opts = DiskOptions::new(&path, cancel);
    let resource = StreamingResource::open_disk(opts).await.unwrap();

    // Empty data should be a no-op
    resource.write_batched(0, &[]).await.unwrap();
    resource.commit(Some(0)).await.unwrap();
}

#[tokio::test]
async fn test_write_batched_at_offset() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("test_batched_offset.bin");
    let cancel = CancellationToken::new();

    let opts = DiskOptions::new(&path, cancel);
    let resource = StreamingResource::open_disk(opts).await.unwrap();

    // Write some initial data
    let initial = b"prefix";
    resource.write_at(0, initial).await.unwrap();

    // Write batched data at offset
    let data: Vec<u8> = (0..128 * 1024).map(|i| (i % 256) as u8).collect();
    resource
        .write_batched(initial.len() as u64, &data)
        .await
        .unwrap();

    let total_len = initial.len() + data.len();
    resource.commit(Some(total_len as u64)).await.unwrap();

    // Verify initial data
    let mut read_buf = vec![0u8; initial.len()];
    let bytes_read = resource.read_at(0, &mut read_buf).await.unwrap();
    assert_eq!(bytes_read, initial.len());
    assert_eq!(&read_buf, initial);

    // Verify batched data
    let mut read_buf = vec![0u8; data.len()];
    let bytes_read = resource
        .read_at(initial.len() as u64, &mut read_buf)
        .await
        .unwrap();
    assert_eq!(bytes_read, data.len());
    assert_eq!(read_buf, data);
}
