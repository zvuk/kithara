use std::time::Duration;

use kithara_storage::{ResourceExt, StorageError, StorageOptions, StorageResource};
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
#[test]
fn atomic_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("test.dat");
    let atomic = StorageResource::open(StorageOptions {
        path: file_path.clone(),
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    assert_eq!(atomic.path(), file_path);

    atomic
        .write_all(b"test data")
        .expect("write should succeed");
    assert_eq!(atomic.path(), file_path);
}

#[rstest]
#[case("simple data", b"Hello, World!")]
#[case("binary data", &[0x00, 0xFF, 0x80, 0x7F])]
#[case("large data", &[0x42; 1024 * 1024])] // 1MB
#[timeout(Duration::from_secs(10))]
#[test]
fn atomic_resource_write_read_success(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] test_name: &str,
    #[case] test_data: &[u8],
) {
    let file_path = temp_dir.path().join(format!("{}.dat", test_name));
    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    atomic.write_all(test_data).expect("write should succeed");

    let read_data = atomic.read_all().expect("read should succeed");
    assert_eq!(&*read_data, test_data, "read data should match");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_empty_write_read(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("empty.dat");
    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    // write_all with empty data commits with final_len=0
    atomic.write_all(b"").expect("write should succeed");

    let read_data = atomic.read_all().expect("read should succeed");
    assert!(
        read_data.is_empty(),
        "empty write should produce empty read"
    );
}

#[rstest]
#[case(true)] // file exists initially
#[case(false)] // file doesn't exist initially
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_read_missing_file(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] create_file_first: bool,
) {
    let file_path = temp_dir.path().join("missing.dat");

    if create_file_first {
        let atomic = StorageResource::open(StorageOptions {
            path: file_path.clone(),
            initial_len: None,
            cancel: cancel_token.clone(),
        })
        .unwrap();
        atomic.write_all(b"initial").unwrap();
    }

    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    if create_file_first {
        let data = atomic.read_all().unwrap();
        assert_eq!(&*data, b"initial");
    } else {
        let data = atomic.read_all().expect("read should succeed");
        assert!(data.is_empty(), "missing file should return empty bytes");
    }
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_cancelled_operations(
    temp_dir: TempDir,
    cancel_token_cancelled: CancellationToken,
) {
    let file_path = temp_dir.path().join("cancelled.dat");
    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token_cancelled,
    })
    .unwrap();

    // write_all on cancelled resource: write_at succeeds (empty check passes first),
    // but commit returns Cancelled
    let write_result = atomic.write_all(b"data");
    assert!(
        write_result.is_err(),
        "write_all should fail when cancelled"
    );

    // read_all checks health
    let read_result = atomic.read_all();
    assert!(read_result.is_err(), "read_all should fail when cancelled");

    // commit checks health
    let commit_result = atomic.commit(None);
    assert!(
        matches!(commit_result, Err(StorageError::Cancelled)),
        "commit should return Cancelled"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_fail_propagation(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("failed.dat");
    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    atomic.fail("test failure".to_string());

    // write_all should fail after resource failure
    let write_result = atomic.write_all(b"data");
    assert!(write_result.is_err(), "write_all should fail after fail()");

    // read_all should fail
    let read_result = atomic.read_all();
    assert!(read_result.is_err(), "read_all should fail after fail()");

    // commit should fail
    let commit_result = atomic.commit(None);
    assert!(
        matches!(commit_result, Err(StorageError::Failed(_))),
        "commit should return Failed"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_concurrent_writes(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("concurrent.dat");
    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    let atomic_clone = atomic.clone();
    let handle1 = std::thread::spawn(move || atomic_clone.write_all(b"data1"));

    let atomic_clone = atomic.clone();
    let handle2 = std::thread::spawn(move || atomic_clone.write_all(b"data2"));

    let result1 = handle1.join().unwrap();
    let result2 = handle2.join().unwrap();
    assert!(result1.is_ok());
    assert!(result2.is_ok());

    let final_data = atomic.read_all().unwrap();
    assert!(*final_data == *b"data1" || *final_data == *b"data2");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_invalid_path(temp_dir: TempDir, cancel_token: CancellationToken) {
    let invalid_path = temp_dir.path().join("nonexistent").join("file.dat");
    let atomic = StorageResource::open(StorageOptions {
        path: invalid_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    let result = atomic.write_all(b"data");
    assert!(result.is_ok(), "write should create parent dirs");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_large_file_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("large.dat");
    let cancel_token = CancellationToken::new();

    let atomic = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();

    let large_data = vec![0x42; 10 * 1024 * 1024];
    atomic.write_all(&large_data).unwrap();

    let read_data = atomic.read_all().unwrap();
    assert_eq!(read_data.len(), large_data.len());
    assert_eq!(&*read_data, large_data.as_slice());
}

#[rstest]
#[case("persist_small", b"persist me")]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_persists_across_reopen(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] name: &str,
    #[case] payload: &[u8],
) {
    let file_path = temp_dir.path().join(format!("{name}.dat"));

    {
        let atomic = StorageResource::open(StorageOptions {
            path: file_path.clone(),
            initial_len: None,
            cancel: cancel_token.clone(),
        })
        .unwrap();
        atomic.write_all(payload).unwrap();
    }

    let reopened = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();
    let data = reopened.read_all().unwrap();
    assert_eq!(&*data, payload);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_empty_persists_across_reopen(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let file_path = temp_dir.path().join("persist_empty.dat");

    {
        let atomic = StorageResource::open(StorageOptions {
            path: file_path.clone(),
            initial_len: None,
            cancel: cancel_token.clone(),
        })
        .unwrap();
        atomic.write_all(b"").unwrap();
    }

    let reopened = StorageResource::open(StorageOptions {
        path: file_path,
        initial_len: None,
        cancel: cancel_token,
    })
    .unwrap();
    let data = reopened.read_all().unwrap();
    assert!(data.is_empty());
}
