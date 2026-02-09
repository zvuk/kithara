use std::time::Duration;

use kithara_bufpool::byte_pool;
use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt, StorageError};
use rstest::*;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::common::fixtures::{cancel_token, cancel_token_cancelled, temp_dir};

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("test.dat");
    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path.clone(),
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    assert_eq!(atomic.path(), Some(file_path.as_path()));

    atomic
        .write_all(b"test data")
        .expect("write should succeed");
    assert_eq!(atomic.path(), Some(file_path.as_path()));
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
    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    atomic.write_all(test_data).expect("write should succeed");

    let mut buf = byte_pool().get();
    atomic.read_into(&mut buf).expect("read should succeed");
    assert_eq!(&*buf, test_data, "read data should match");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_empty_write_read(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("empty.dat");
    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // write_all with empty data commits with final_len=0
    atomic.write_all(b"").expect("write should succeed");

    let mut buf = byte_pool().get();
    let n = atomic.read_into(&mut buf).expect("read should succeed");
    assert_eq!(n, 0, "empty write should produce empty read");
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
        let atomic: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions {
                path: file_path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();
        atomic.write_all(b"initial").unwrap();
    }

    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let mut buf = byte_pool().get();
    if create_file_first {
        atomic.read_into(&mut buf).unwrap();
        assert_eq!(&*buf, b"initial");
    } else {
        let n = atomic.read_into(&mut buf).expect("read should succeed");
        assert_eq!(n, 0, "missing file should return empty bytes");
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
    let atomic: MmapResource = Resource::open(
        cancel_token_cancelled,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // write_all on cancelled resource: write_at succeeds (empty check passes first),
    // but commit returns Cancelled
    let write_result = atomic.write_all(b"data");
    assert!(
        write_result.is_err(),
        "write_all should fail when cancelled"
    );

    // read_into checks health
    let mut buf = byte_pool().get();
    let read_result = atomic.read_into(&mut buf);
    assert!(read_result.is_err(), "read_into should fail when cancelled");

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
    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    atomic.fail("test failure".to_string());

    // write_all should fail after resource failure
    let write_result = atomic.write_all(b"data");
    assert!(write_result.is_err(), "write_all should fail after fail()");

    // read_into should fail
    let mut buf = byte_pool().get();
    let read_result = atomic.read_into(&mut buf);
    assert!(read_result.is_err(), "read_into should fail after fail()");

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
    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let atomic_clone = atomic.clone();
    let handle1 = std::thread::spawn(move || atomic_clone.write_all(b"data1"));

    let atomic_clone = atomic.clone();
    let handle2 = std::thread::spawn(move || atomic_clone.write_all(b"data2"));

    let result1 = handle1.join().unwrap();
    let result2 = handle2.join().unwrap();

    // write_all = write_at + commit (two steps, not atomic).
    // When one thread commits first, the other's write_at may fail
    // because Auto mode rejects writes to a committed resource.
    // At least one must succeed; both succeeding is also valid.
    assert!(
        result1.is_ok() || result2.is_ok(),
        "at least one concurrent write_all must succeed"
    );

    let mut buf = byte_pool().get();
    atomic.read_into(&mut buf).unwrap();
    assert!(*buf == *b"data1" || *buf == *b"data2");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn atomic_resource_invalid_path(temp_dir: TempDir, cancel_token: CancellationToken) {
    let invalid_path = temp_dir.path().join("nonexistent").join("file.dat");
    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: invalid_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
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

    let atomic: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let large_data = vec![0x42; 10 * 1024 * 1024];
    atomic.write_all(&large_data).unwrap();

    let mut buf = byte_pool().get();
    atomic.read_into(&mut buf).unwrap();
    assert_eq!(buf.len(), large_data.len());
    assert_eq!(&*buf, large_data.as_slice());
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
        let atomic: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions {
                path: file_path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();
        atomic.write_all(payload).unwrap();
    }

    let reopened: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();
    let mut buf = byte_pool().get();
    reopened.read_into(&mut buf).unwrap();
    assert_eq!(&*buf, payload);
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
        let atomic: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions {
                path: file_path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();
        atomic.write_all(b"").unwrap();
    }

    let reopened: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();
    let mut buf = byte_pool().get();
    let n = reopened.read_into(&mut buf).unwrap();
    assert_eq!(n, 0);
}
