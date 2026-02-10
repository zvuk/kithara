// StreamingResource tests (merged from edge cases)
use std::time::Duration;

use kithara_storage::{
    MmapOptions, MmapResource, OpenMode, Resource, ResourceExt, ResourceStatus, StorageError,
    WaitOutcome,
};
use rstest::*;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::common::fixtures::{cancel_token, temp_dir};

/// Helper to read bytes from resource into a new Vec
fn read_bytes<R: ResourceExt>(res: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let n = res.read_at(offset, &mut buf).unwrap_or(0);
    buf.truncate(n);
    buf
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_path_method(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("streaming.dat");
    let streaming: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path.clone(),
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .expect("open should succeed");

    assert_eq!(streaming.path(), Some(file_path.as_path()));

    streaming
        .write_at(0, b"test")
        .expect("write should succeed");
    assert_eq!(streaming.path(), Some(file_path.as_path()));
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[test]
fn streaming_resource_open_and_status_new(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("stream.dat");

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // A brand new resource should be Active
    assert_eq!(resource.status(), ResourceStatus::Active);
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[test]
fn streaming_resource_open_existing_is_committed(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let file_path = temp_dir.path().join("stream.dat");

    // Create and commit a resource first
    {
        let resource: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions {
                path: file_path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();
        resource.write_at(0, b"existing data").unwrap();
        resource.commit(Some(13)).unwrap();
    }

    // Reopen — should be Committed
    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    assert_eq!(
        resource.status(),
        ResourceStatus::Committed {
            final_len: Some(13)
        }
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_range_write_wait_read(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("ranges.dat");
    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"Hello, ").unwrap();
    resource.write_at(7, b"World!").unwrap();

    resource.wait_range(0..13).unwrap();
    let data = read_bytes(&resource, 0, 13);
    assert_eq!(&data, b"Hello, World!");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_sparse_file_behavior() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sparse.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(100, b"start").unwrap();
    resource.write_at(1000, b"middle").unwrap();
    resource.write_at(10000, b"end").unwrap();

    resource.wait_range(100..105).unwrap();
    resource.wait_range(1000..1006).unwrap();
    resource.wait_range(10000..10003).unwrap();

    assert_eq!(&*read_bytes(&resource, 100, 5), b"start");
    assert_eq!(&*read_bytes(&resource, 1000, 6), b"middle");
    assert_eq!(&*read_bytes(&resource, 10000, 3), b"end");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_overlapping_writes() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("overlap.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"Hello World!").unwrap();
    resource.write_at(6, b"Kithara!").unwrap();

    resource.wait_range(0..14).unwrap();
    let data = read_bytes(&resource, 0, 14);
    assert_eq!(&data, b"Hello Kithara!");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_zero_length_commit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("zero.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.commit(Some(0)).unwrap();

    let status = resource.status();
    assert_eq!(status, ResourceStatus::Committed { final_len: Some(0) });

    let outcome = resource.wait_range(0..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_edge_case_ranges() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("edges.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"X").unwrap();

    resource.wait_range(0..1).unwrap();
    let data = read_bytes(&resource, 0, 1);
    assert_eq!(&data, b"X");

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());

    resource.commit(Some(1)).unwrap();
    let data = read_bytes(&resource, 0, 10);
    assert_eq!(&data, b"X");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_concurrent_wait_and_write() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("concurrent_wait.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let resource_clone = resource.clone();
    let wait_handle = std::thread::spawn(move || resource_clone.wait_range(0..10));

    std::thread::sleep(Duration::from_millis(10));

    resource.write_at(0, b"0123456789").unwrap();

    let wait_result = wait_handle.join().unwrap().unwrap();
    assert_eq!(wait_result, WaitOutcome::Ready);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_persists_across_reopen() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("reopen.dat");
    let cancel_token = CancellationToken::new();

    {
        let resource: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions {
                path: file_path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();

        resource.write_at(0, b"persisted data").unwrap();
        resource.commit(Some(14)).unwrap();
        resource.wait_range(0..14).unwrap();

        let data = read_bytes(&resource, 0, 14);
        assert_eq!(&data, b"persisted data");
    }

    let file_len = std::fs::metadata(&file_path).unwrap().len();
    let resource: MmapResource = Resource::open(
        CancellationToken::new(),
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let data = read_bytes(&resource, 0, file_len as usize);
    assert_eq!(&data, b"persisted data");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_wait_after_reopen() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("wait_reopen.dat");
    let cancel_token = CancellationToken::new();
    let payload = b"waited bytes";

    {
        let resource: MmapResource = Resource::open(
            cancel_token.clone(),
            MmapOptions {
                path: file_path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();

        resource.write_at(0, payload).unwrap();
        resource.commit(Some(payload.len() as u64)).unwrap();
        resource.wait_range(0..payload.len() as u64).unwrap();
    }

    let file_len = std::fs::metadata(&file_path).unwrap().len();
    let resource: MmapResource = Resource::open(
        CancellationToken::new(),
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let outcome = resource.wait_range(0..file_len).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let data = read_bytes(&resource, 0, file_len as usize);
    assert_eq!(&data, payload);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_wait_range_partial_coverage() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("partial.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token.clone(),
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"Hello").unwrap();

    let resource_clone = resource.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = resource_clone.wait_range(0..10);
        let _ = tx.send(result);
    });

    // Should not complete within 100ms (only 5 bytes written, need 10)
    let wait_result = rx.recv_timeout(Duration::from_millis(100));
    assert!(wait_result.is_err()); // Timeout

    // Write remaining bytes
    resource.write_at(5, b", World!").unwrap();
    resource.commit(Some(13)).unwrap();

    // Now it should complete
    resource.wait_range(0..13).unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_commit_and_eof(temp_dir: TempDir, cancel_token: CancellationToken) {
    let file_path = temp_dir.path().join("commit.dat");
    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"Hello").unwrap();

    let outcome = resource.wait_range(0..5).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    resource.commit(Some(5)).unwrap();

    let outcome = resource.wait_range(5..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    let data = read_bytes(&resource, 10, 5);
    assert!(data.is_empty());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_commit_without_final_len() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("commit_no_len.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"Hello").unwrap();
    resource.commit(None).unwrap();

    // Commit without final_len means we don't know the total size.
    // read_at still works, but wait_range doesn't know when EOF is reached.
    let status = resource.status();
    assert_eq!(status, ResourceStatus::Committed { final_len: None });

    // We can still read the written data
    let data = read_bytes(&resource, 0, 5);
    assert_eq!(&data, b"Hello");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_sealed_after_commit() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sealed.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // First commit with zero — resource is committed but empty
    resource.commit(Some(0)).unwrap();

    // We can still write after commit (mmap-backed, not sealed)
    resource.write_at(0, b"data").unwrap();
    resource.commit(Some(4)).unwrap();

    let data = read_bytes(&resource, 0, 4);
    assert_eq!(&data, b"data");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_cancel_during_wait() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("cancel_wait.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token.clone(),
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let resource_clone = resource.clone();
    let wait_handle = std::thread::spawn(move || resource_clone.wait_range(0..10));

    std::thread::sleep(Duration::from_millis(50));
    cancel_token.cancel();

    let wait_result = wait_handle.join().unwrap();
    assert!(matches!(wait_result, Err(StorageError::Cancelled)));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_fail_wakes_waiters() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("fail_waiters.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let resource_clone = resource.clone();
    let wait_handle = std::thread::spawn(move || resource_clone.wait_range(0..10));

    let resource_clone = resource.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        resource_clone.fail("test failure".to_string());
    });

    let wait_result = wait_handle.join().unwrap();
    assert!(matches!(wait_result, Err(StorageError::Failed(_))));
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_concurrent_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("concurrent_ops.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    let resource_clone = resource.clone();
    let handle1 = std::thread::spawn(move || resource_clone.write_at(0, b"Hello"));

    let resource_clone = resource.clone();
    let handle2 = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        resource_clone.write_at(5, b"World")
    });

    assert!(handle1.join().unwrap().is_ok());
    assert!(handle2.join().unwrap().is_ok());

    resource.commit(Some(10)).unwrap();

    let outcome = resource.wait_range(0..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let data = read_bytes(&resource, 0, 10);
    assert_eq!(&data[..5], b"Hello");
    assert_eq!(&data[5..], b"World");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_invalid_ranges() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("invalid_ranges.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // Reversed range (start > end) should return InvalidRange
    assert!(matches!(
        resource.wait_range(std::ops::Range { start: 10, end: 5 }),
        Err(StorageError::InvalidRange { start: 10, end: 5 })
    ));

    // Empty range (start == end) is valid and returns Ready
    let outcome = resource.wait_range(5..5).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Overflow in write_at should return error
    let large_data = vec![0u8; 1000];
    let result = resource.write_at(u64::MAX, &large_data);
    assert!(result.is_err());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_whole_object_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("whole_object.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"Hello, World!").unwrap();
    resource.commit(Some(13)).unwrap();

    let status = resource.status();
    assert_eq!(
        status,
        ResourceStatus::Committed {
            final_len: Some(13)
        }
    );

    resource.wait_range(0..13).unwrap();
    let data = read_bytes(&resource, 0, 13);
    assert_eq!(&data, b"Hello, World!");
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_empty_operations() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty_ops.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // Empty write is a no-op
    resource.write_at(0, b"").unwrap();

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());

    // Commit with zero length
    resource.commit(Some(0)).unwrap();

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_complex_range_scenario() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("complex_ranges.dat");
    let cancel_token = CancellationToken::new();

    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: None,
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    resource.write_at(0, b"0123456789").unwrap();
    resource.write_at(20, b"0123456789").unwrap();

    let resource_clone = resource.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = resource_clone.wait_range(0..15);
        let _ = tx.send(result);
    });

    let wait_result = rx.recv_timeout(Duration::from_millis(100));
    assert!(wait_result.is_err()); // Timeout — gap at 10..15 not yet written

    resource.write_at(10, b"ABCDEFGHIJ").unwrap();

    resource.wait_range(0..30).unwrap();

    let data = read_bytes(&resource, 0, 30);
    assert_eq!(&data[0..10], b"0123456789");
    assert_eq!(&data[10..20], b"ABCDEFGHIJ");
    assert_eq!(&data[20..30], b"0123456789");

    resource.commit(Some(30)).unwrap();

    let outcome = resource.wait_range(30..40).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[test]
fn streaming_resource_initial_len_hint() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("initial_hint.dat");
    let cancel_token = CancellationToken::new();

    // initial_len is a hint for backing file size, not data availability
    let resource: MmapResource = Resource::open(
        cancel_token,
        MmapOptions {
            path: file_path,
            initial_len: Some(100),
            mode: OpenMode::Auto,
        },
    )
    .unwrap();

    // Resource is Active (not committed), no data available yet
    assert_eq!(resource.status(), ResourceStatus::Active);

    // Write actual data and commit
    resource.write_at(0, b"real data").unwrap();
    resource.commit(Some(9)).unwrap();

    let data = read_bytes(&resource, 0, 9);
    assert_eq!(&data, b"real data");
}
