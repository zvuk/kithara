#[cfg(target_arch = "wasm32")]
use kithara::platform::thread;
#[cfg(target_arch = "wasm32")]
use kithara::storage::MemResource;
#[cfg(not(target_arch = "wasm32"))]
use kithara::storage::{MmapOptions, MmapResource, Resource};
use kithara::{
    bufpool::BytePool,
    platform::{CancelToken, time::Duration, tokio::task::spawn_blocking},
    storage::{ResourceRead, StorageError, StorageResource},
};
use kithara_integration_tests::{TestTempDir, cancel_token, cancel_token_cancelled, temp_dir};

#[cfg(not(target_arch = "wasm32"))]
type TestResource = MmapResource;
#[cfg(target_arch = "wasm32")]
type TestResource = MemResource;

fn open_test_resource(temp_dir: &TestTempDir, name: &str, cancel: CancelToken) -> TestResource {
    #[cfg(not(target_arch = "wasm32"))]
    {
        open_mmap_at(temp_dir.path().join(name), cancel)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (temp_dir, name);
        MemResource::new(cancel)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn open_mmap_at(path: std::path::PathBuf, cancel: CancelToken) -> MmapResource {
    Resource::open(cancel, MmapOptions::new(path)).expect("open should succeed")
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn atomic_resource_path_method(temp_dir: TestTempDir, cancel_token: CancelToken) {
    let file_path = temp_dir.path().join("test.dat");
    let atomic = open_mmap_at(file_path.clone(), cancel_token);

    assert_eq!(atomic.path(), Some(file_path.as_path()));

    let atomic = atomic
        .write_all(b"test data")
        .expect("write should succeed");
    assert_eq!(atomic.path(), Some(file_path.as_path()));
}

#[kithara::test(timeout(Duration::from_secs(10)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
#[case("simple data", b"Hello, World!")]
#[case("binary data", &[0x00, 0xFF, 0x80, 0x7F])]
#[case("large data", &[0x42; 1024 * 1024])]
fn atomic_resource_write_read_success(
    temp_dir: TestTempDir,
    cancel_token: CancelToken,
    #[case] test_name: &str,
    #[case] test_data: &[u8],
) {
    let atomic = open_test_resource(&temp_dir, &format!("{}.dat", test_name), cancel_token);

    let atomic = atomic.write_all(test_data).expect("write should succeed");

    let mut buf = BytePool::default().get();
    atomic.read_into(&mut buf).expect("read should succeed");
    assert_eq!(&*buf, test_data, "read data should match");
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn atomic_resource_empty_write_read(temp_dir: TestTempDir, cancel_token: CancelToken) {
    let atomic = open_test_resource(&temp_dir, "empty.dat", cancel_token);

    let atomic = atomic.write_all(b"").expect("write should succeed");

    let mut buf = BytePool::default().get();
    let n = atomic.read_into(&mut buf).expect("read should succeed");
    assert_eq!(n, 0, "empty write should produce empty read");
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(true)]
#[case(false)]
fn atomic_resource_read_missing_file(
    temp_dir: TestTempDir,
    cancel_token: CancelToken,
    #[case] create_file_first: bool,
) {
    let file_path = temp_dir.path().join("missing.dat");

    if create_file_first {
        let atomic = open_mmap_at(file_path.clone(), cancel_token.clone());
        atomic.write_all(b"initial").unwrap();
    }

    let atomic = open_mmap_at(file_path, cancel_token);

    let mut buf = BytePool::default().get();
    if create_file_first {
        atomic.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, b"initial");
    } else {
        let n = atomic.read_into(&mut buf).expect("read should succeed");
        assert_eq!(n, 0, "missing file should return empty bytes");
    }
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn atomic_resource_cancelled_operations(
    temp_dir: TestTempDir,
    cancel_token_cancelled: CancelToken,
) {
    let atomic = open_test_resource(&temp_dir, "cancelled.dat", cancel_token_cancelled);
    let reader = atomic.reader();

    let write_result = atomic.write_at(0, b"data");
    assert!(write_result.is_err(), "write_at should fail when cancelled");

    let mut buf = BytePool::default().get();
    let read_result = reader.read_into(&mut buf);
    assert!(read_result.is_err(), "read_into should fail when cancelled");

    let commit_result = atomic.commit(None);
    assert!(
        matches!(commit_result, Err(StorageError::Cancelled)),
        "commit should return Cancelled"
    );
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn atomic_resource_fail_propagation(temp_dir: TestTempDir, cancel_token: CancelToken) {
    let atomic = open_test_resource(&temp_dir, "failed.dat", cancel_token);
    let reader = atomic.reader();

    // `fail` consumes the writer: writing/committing after a failure is a
    // compile error now (the writer is gone), not a runtime rejection. The
    // reader observes the failure.
    atomic.fail("test failure".to_string());

    let mut buf = BytePool::default().get();
    let read_result = reader.read_into(&mut buf);
    assert!(read_result.is_err(), "read_into should fail after fail()");
    let mut probe = [0u8; 4];
    assert!(
        matches!(reader.read_at(0, &mut probe), Err(StorageError::Failed(_))),
        "read should surface Failed"
    );
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn atomic_resource_concurrent_writes(temp_dir: TestTempDir, cancel_token: CancelToken) {
    let atomic = StorageResource::from(open_test_resource(
        &temp_dir,
        "concurrent.dat",
        cancel_token,
    ));

    let atomic_clone = atomic.clone();
    let handle1 = spawn_blocking(move || atomic_clone.write_all(b"data1"));

    let atomic_clone = atomic.clone();
    let handle2 = spawn_blocking(move || atomic_clone.write_all(b"data2"));

    let result1 = handle1.await.unwrap();
    let result2 = handle2.await.unwrap();

    assert!(
        result1.is_ok() || result2.is_ok(),
        "at least one concurrent write_all must succeed"
    );

    let mut buf = BytePool::default().get();
    atomic.read_into(&mut buf).unwrap();
    assert!(*buf == *b"data1" || *buf == *b"data2");
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn atomic_resource_invalid_path(temp_dir: TestTempDir, cancel_token: CancelToken) {
    let invalid_path = temp_dir.path().join("nonexistent").join("file.dat");
    let atomic = open_mmap_at(invalid_path, cancel_token);

    let result = atomic.write_all(b"data");
    assert!(result.is_ok(), "write should create parent dirs");
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn atomic_resource_large_file_operations() {
    let temp_dir = TestTempDir::new();
    let file_path = temp_dir.path().join("large.dat");
    let cancel_token = CancelToken::never();

    let atomic = open_mmap_at(file_path, cancel_token);

    let large_data = vec![0x42; 10 * 1024 * 1024];
    let atomic = atomic.write_all(&large_data).unwrap();

    let mut buf = BytePool::default().get();
    atomic.read_into(&mut buf).unwrap();
    assert_eq!(buf.len(), large_data.len());
    assert_eq!(&*buf, large_data.as_slice());
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::small("persist_small", b"persist me")]
#[case::empty("persist_empty", b"")]
fn atomic_resource_persists_across_reopen(
    temp_dir: TestTempDir,
    cancel_token: CancelToken,
    #[case] name: &str,
    #[case] payload: &[u8],
) {
    let file_path = temp_dir.path().join(format!("{name}.dat"));

    {
        let atomic = open_mmap_at(file_path.clone(), cancel_token.clone());
        atomic.write_all(payload).unwrap();
    }

    let reopened = open_mmap_at(file_path, cancel_token);
    let mut buf = BytePool::default().get();
    let n = reopened.read_into(&mut buf).unwrap();
    assert_eq!(n, payload.len(), "byte count should match written payload");
    assert_eq!(&*buf, payload, "round-tripped bytes should match payload");
}
