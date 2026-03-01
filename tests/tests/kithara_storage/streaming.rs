// StreamingResource tests (merged from edge cases)
#[cfg(target_arch = "wasm32")]
use kithara::storage::MemResource;
#[cfg(not(target_arch = "wasm32"))]
use kithara::storage::Resource;
#[cfg(not(target_arch = "wasm32"))]
use kithara::storage::{MmapOptions, MmapResource, OpenMode};
use kithara::storage::{ResourceExt, ResourceStatus, StorageError, WaitOutcome};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::{TestTempDir, cancel_token, temp_dir};
use tokio_util::sync::CancellationToken;

#[cfg(not(target_arch = "wasm32"))]
type TestResource = MmapResource;
#[cfg(target_arch = "wasm32")]
type TestResource = MemResource;

fn open_test_resource(
    temp_dir: &TestTempDir,
    name: &str,
    cancel: CancellationToken,
) -> TestResource {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Resource::open(
            cancel,
            MmapOptions {
                path: temp_dir.path().join(name),
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .expect("open should succeed")
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (temp_dir, name);
        MemResource::new(cancel)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn open_test_resource_with_len(
    temp_dir: &TestTempDir,
    name: &str,
    len: u64,
    cancel: CancellationToken,
) -> TestResource {
    Resource::open(
        cancel,
        MmapOptions {
            path: temp_dir.path().join(name),
            initial_len: Some(len),
            mode: OpenMode::Auto,
        },
    )
    .expect("open should succeed")
}

/// Helper to read bytes from resource into a new Vec.
fn read_bytes<R: ResourceExt>(res: &R, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let n = res.read_at(offset, &mut buf).unwrap_or(0);
    buf.truncate(n);
    buf
}

fn assert_wait_times_out<T>(handle: &kithara_platform::thread::JoinHandle<T>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if handle.is_finished() {
            panic!("wait_range completed before expected timeout");
        }
        kithara_platform::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn assert_wait_finishes<T>(handle: &kithara_platform::thread::JoinHandle<T>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if handle.is_finished() {
            return;
        }
        kithara_platform::thread::sleep(Duration::from_millis(1));
    }
    panic!("wait_range did not wake within expected timeout");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_path_method(temp_dir: TestTempDir, cancel_token: CancellationToken) {
    #[cfg(not(target_arch = "wasm32"))]
    {
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

    #[cfg(target_arch = "wasm32")]
    {
        let streaming = MemResource::new(cancel_token);
        assert_eq!(streaming.path(), None);

        streaming
            .write_at(0, b"test")
            .expect("write should succeed");
        assert_eq!(streaming.path(), None);
        let _ = temp_dir;
    }
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn streaming_resource_open_and_status_new(temp_dir: TestTempDir, cancel_token: CancellationToken) {
    let resource = open_test_resource(&temp_dir, "stream.dat", cancel_token);

    // A brand new resource should be Active
    assert_eq!(resource.status(), ResourceStatus::Active);
}

#[kithara::test(timeout(Duration::from_secs(10)))]
fn streaming_resource_open_existing_is_committed(
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let file_path = temp_dir.path().join("stream.dat");

        // Create and commit a resource first.
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

    #[cfg(target_arch = "wasm32")]
    {
        let resource = MemResource::from_bytes(b"existing data", cancel_token);
        let data = read_bytes(&resource, 0, 13);
        assert_eq!(
            resource.status(),
            ResourceStatus::Committed {
                final_len: Some(13)
            }
        );
        assert_eq!(data, b"existing data");
        let _ = temp_dir;
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_range_write_wait_read(
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) {
    let resource = open_test_resource(&temp_dir, "ranges.dat", cancel_token);

    resource.write_at(0, b"Hello, ").unwrap();
    resource.write_at(7, b"World!").unwrap();

    resource.wait_range(0..13).unwrap();
    let data = read_bytes(&resource, 0, 13);
    assert_eq!(&data, b"Hello, World!");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_sparse_file_behavior() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "sparse.dat", cancel_token);

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

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_overlapping_writes() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "overlap.dat", cancel_token);

    resource.write_at(0, b"Hello World!").unwrap();
    resource.write_at(6, b"Kithara!").unwrap();

    resource.wait_range(0..14).unwrap();
    let data = read_bytes(&resource, 0, 14);
    assert_eq!(&data, b"Hello Kithara!");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_zero_length_commit() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "zero.dat", cancel_token);

    resource.commit(Some(0)).unwrap();

    let status = resource.status();
    assert_eq!(status, ResourceStatus::Committed { final_len: Some(0) });

    let outcome = resource.wait_range(0..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_edge_case_ranges() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "edges.dat", cancel_token);

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

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_concurrent_wait_and_write() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "concurrent_wait.dat", cancel_token);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let resource_clone = resource.clone();
        let wait_handle = kithara_platform::thread::spawn(move || resource_clone.wait_range(0..10));

        kithara_platform::thread::sleep(Duration::from_millis(10));

        resource.write_at(0, b"0123456789").unwrap();

        let wait_result = wait_handle.join().unwrap().unwrap();
        assert_eq!(wait_result, WaitOutcome::Ready);
        return;
    }

    #[cfg(target_arch = "wasm32")]
    {
        resource.write_at(0, b"0123456789").unwrap();
        assert_eq!(resource.wait_range(0..10).unwrap(), WaitOutcome::Ready);
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_persists_across_reopen() {
    let _temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    #[cfg(not(target_arch = "wasm32"))]
    {
        {
            let file_path = _temp_dir.path().join("reopen.dat");
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

        let file_path = _temp_dir.path().join("reopen.dat");
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

    #[cfg(target_arch = "wasm32")]
    {
        let resource = MemResource::from_bytes(b"persisted data", cancel_token);
        let data = read_bytes(&resource, 0, 14);
        assert_eq!(&data, b"persisted data");
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_wait_after_reopen() {
    let _temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();
    let payload = b"waited bytes";

    #[cfg(not(target_arch = "wasm32"))]
    {
        {
            let file_path = _temp_dir.path().join("wait_reopen.dat");
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

        let file_path = _temp_dir.path().join("wait_reopen.dat");
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

    #[cfg(target_arch = "wasm32")]
    {
        let resource = MemResource::from_bytes(payload, cancel_token);
        let outcome = resource.wait_range(0..payload.len() as u64).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
        let data = read_bytes(&resource, 0, payload.len());
        assert_eq!(&data, payload);
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_wait_range_partial_coverage() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "partial.dat", cancel_token.clone());

    resource.write_at(0, b"Hello").unwrap();

    let resource_clone = resource.clone();
    let wait_handle = kithara_platform::thread::spawn(move || resource_clone.wait_range(0..10));

    // Should not complete within 100ms (only 5 bytes written, need 10)
    assert_wait_times_out(&wait_handle, Duration::from_millis(100));

    // Write remaining bytes
    resource.write_at(5, b", World!").unwrap();
    resource.commit(Some(13)).unwrap();

    // Now it should complete
    resource.wait_range(0..13).unwrap();
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_commit_and_eof(temp_dir: TestTempDir, cancel_token: CancellationToken) {
    let resource = open_test_resource(&temp_dir, "commit.dat", cancel_token);

    resource.write_at(0, b"Hello").unwrap();

    let outcome = resource.wait_range(0..5).unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    resource.commit(Some(5)).unwrap();

    let outcome = resource.wait_range(5..10).unwrap();
    assert_eq!(outcome, WaitOutcome::Eof);

    let data = read_bytes(&resource, 10, 5);
    assert!(data.is_empty());
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_commit_without_final_len() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "commit_no_len.dat", cancel_token);

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

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_sealed_after_commit() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "sealed.dat", cancel_token);

    // First commit with zero — resource is committed but empty
    resource.commit(Some(0)).unwrap();

    if resource.write_at(0, b"data").is_err() {
        resource.reactivate().unwrap();
        resource.write_at(0, b"data").unwrap();
    }
    resource.commit(Some(4)).unwrap();

    let data = read_bytes(&resource, 0, 4);
    assert_eq!(&data, b"data");
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_cancel_during_wait() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "cancel_wait.dat", cancel_token.clone());

    #[cfg(not(target_arch = "wasm32"))]
    {
        let resource_clone = resource.clone();
        let wait_handle = kithara_platform::thread::spawn(move || resource_clone.wait_range(0..10));

        kithara_platform::thread::sleep(Duration::from_millis(50));
        cancel_token.cancel();

        let wait_result = wait_handle.join().unwrap();
        assert!(matches!(wait_result, Err(StorageError::Cancelled)));
        return;
    }

    #[cfg(target_arch = "wasm32")]
    {
        cancel_token.cancel();
        let wait_result = resource.wait_range(0..10);
        assert!(matches!(wait_result, Err(StorageError::Cancelled)));
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_fail_wakes_waiters() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "fail_waiters.dat", cancel_token);
    let resource_clone = resource.clone();
    resource.fail("test failure".to_string());
    assert!(matches!(resource.status(), ResourceStatus::Failed(_)));

    #[cfg(not(target_arch = "wasm32"))]
    {
        let wait_handle = kithara_platform::thread::spawn(move || resource_clone.wait_range(0..10));
        assert_wait_finishes(&wait_handle, Duration::from_secs(1));
        let wait_result = wait_handle.join().unwrap();
        assert!(matches!(wait_result, Err(StorageError::Failed(_))));
    }

    #[cfg(target_arch = "wasm32")]
    {
        let wait_result = resource_clone.wait_range(0..10);
        assert!(matches!(wait_result, Err(StorageError::Failed(_))));
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_concurrent_operations() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "concurrent_ops.dat", cancel_token);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let resource_clone = resource.clone();
        let handle1 = kithara_platform::thread::spawn(move || resource_clone.write_at(0, b"Hello"));

        let resource_clone = resource.clone();
        let handle2 = kithara_platform::thread::spawn(move || {
            kithara_platform::thread::sleep(Duration::from_millis(10));
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
        return;
    }

    #[cfg(target_arch = "wasm32")]
    {
        resource.write_at(0, b"Hello").unwrap();
        resource.write_at(5, b"World").unwrap();
        resource.commit(Some(10)).unwrap();

        let outcome = resource.wait_range(0..10).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);

        let data = read_bytes(&resource, 0, 10);
        assert_eq!(&data[..5], b"Hello");
        assert_eq!(&data[5..], b"World");
    }
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_invalid_ranges() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "invalid_ranges.dat", cancel_token);

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

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_whole_object_operations() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "whole_object.dat", cancel_token);

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

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_empty_operations() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "empty_ops.dat", cancel_token);

    // Empty write is a no-op
    resource.write_at(0, b"").unwrap();

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());

    // Commit with zero length
    resource.commit(Some(0)).unwrap();

    let data = read_bytes(&resource, 0, 0);
    assert!(data.is_empty());
}

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_complex_range_scenario() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    let resource = open_test_resource(&temp_dir, "complex_ranges.dat", cancel_token);

    resource.write_at(0, b"0123456789").unwrap();
    resource.write_at(20, b"0123456789").unwrap();

    let resource_clone = resource.clone();
    let wait_handle = kithara_platform::thread::spawn(move || resource_clone.wait_range(0..15));

    assert_wait_times_out(&wait_handle, Duration::from_millis(100));

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

#[kithara::test(timeout(Duration::from_secs(5)))]
fn streaming_resource_initial_len_hint() {
    let temp_dir = TestTempDir::new();
    let cancel_token = CancellationToken::new();

    // initial_len is a hint for backing file size, not data availability.
    #[cfg(not(target_arch = "wasm32"))]
    let resource = open_test_resource_with_len(&temp_dir, "initial_hint.dat", 100, cancel_token);

    #[cfg(target_arch = "wasm32")]
    let resource = {
        let resource = open_test_resource(&temp_dir, "initial_hint.dat", cancel_token);
        assert_eq!(resource.status(), ResourceStatus::Active);
        resource
    };

    // Write actual data and commit.
    resource.write_at(0, b"real data").unwrap();
    resource.commit(Some(9)).unwrap();

    let data = read_bytes(&resource, 0, 9);
    assert_eq!(&data, b"real data");
}
