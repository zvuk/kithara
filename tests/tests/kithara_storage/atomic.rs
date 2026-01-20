use std::time::Duration;

use kithara_storage::{AtomicOptions, AtomicResource, Resource, StorageError};
use rstest::*;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

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

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("test.dat");
    let atomic: AtomicResource =
        AtomicResource::open(AtomicOptions::new(file_path.clone(), cancel_token));

    assert_eq!(atomic.path(), file_path);

    atomic
        .write(b"test data")
        .await
        .expect("write should succeed");
    assert_eq!(atomic.path(), file_path);
}

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

    atomic.write(test_data).await.expect("write should succeed");

    let read_data = atomic.read().await.expect("read should succeed");
    assert_eq!(&*read_data, test_data, "read data should match");
}

#[rstest]
#[case(true)] // file exists initially
#[case(false)] // file doesn't exist initially
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_read_missing_file(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] create_file_first: bool,
) {
    let file_path = temp_dir.path().join("missing.dat");

    if create_file_first {
        let atomic: AtomicResource =
            AtomicResource::open(AtomicOptions::new(&file_path, cancel_token.clone()));
        atomic.write(b"initial").await.unwrap();
    }

    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    if create_file_first {
        let data = atomic.read().await.unwrap();
        assert_eq!(&*data, b"initial");
    } else {
        let data = atomic.read().await.expect("read should succeed");
        assert!(data.is_empty(), "missing file should return empty bytes");
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

    atomic
        .fail("test failure")
        .await
        .expect("fail should succeed");

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
    let handle1 = tokio::spawn(async move { atomic_clone.write(b"data1").await });

    let atomic_clone = atomic.clone();
    let handle2 = tokio::spawn(async move { atomic_clone.write(b"data2").await });

    let (result1, result2) = tokio::join!(handle1, handle2);
    assert!(result1.unwrap().is_ok());
    assert!(result2.unwrap().is_ok());

    let final_data = atomic.read().await.unwrap();
    assert!(*final_data == *b"data1" || *final_data == *b"data2");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_invalid_path(temp_dir: TempDir, cancel_token: CancellationToken) {
    let invalid_path = temp_dir.path().join("nonexistent").join("file.dat");
    let atomic: AtomicResource =
        AtomicResource::open(AtomicOptions::new(invalid_path, cancel_token));

    let result = atomic.write(b"data").await;
    assert!(result.is_ok(), "write should create parent dirs");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_large_file_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("large.dat");
    let cancel_token = CancellationToken::new();

    let atomic: AtomicResource = AtomicResource::open(AtomicOptions::new(file_path, cancel_token));

    let large_data = vec![0x42; 10 * 1024 * 1024];
    atomic.write(&large_data).await.unwrap();

    let read_data = atomic.read().await.unwrap();
    assert_eq!(read_data.len(), large_data.len());
    assert_eq!(&*read_data, large_data.as_slice());
}

#[rstest]
#[case("persist_small", b"persist me")]
#[case("persist_empty", b"")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn atomic_resource_persists_across_reopen(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] name: &str,
    #[case] payload: &[u8],
) {
    let file_path = temp_dir.path().join(format!("{name}.dat"));

    {
        let atomic: AtomicResource =
            AtomicResource::open(AtomicOptions::new(file_path.clone(), cancel_token.clone()));
        atomic.write(payload).await.unwrap();
    }

    let reopened: AtomicResource =
        AtomicResource::open(AtomicOptions::new(file_path, cancel_token));
    let data = reopened.read().await.unwrap();
    assert_eq!(&*data, payload);
}
