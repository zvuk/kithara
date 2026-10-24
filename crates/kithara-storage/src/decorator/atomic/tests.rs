#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Barrier, mpsc};

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::thread;
use kithara_platform::{CancelToken, time::Duration};
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_utils::cancel_token;
#[cfg(not(target_arch = "wasm32"))]
use tempfile::TempDir;

use super::core::Atomic;
use crate::{
    MemDriver, MemOptions,
    test_pools::{byte_buffer, pools, pools_with_budget},
};
#[cfg(not(target_arch = "wasm32"))]
use crate::{MmapDriver, MmapOptions, OpenMode, ResourceRead, StorageError};

#[cfg(not(target_arch = "wasm32"))]
fn create_mmap_resource(dir: &TempDir, name: &str) -> Atomic<MmapDriver> {
    let path = dir.path().join(name);
    Atomic::open(
        CancelToken::never(),
        MmapOptions::for_path(path)
            .mode(OpenMode::ReadWrite)
            .initial_len(4096)
            .build(),
    )
    .unwrap()
}

fn create_mem_resource() -> Atomic<MemDriver> {
    let pools = pools();
    Atomic::open(
        CancelToken::never(),
        MemOptions::builder().buffer(byte_buffer(&pools)).build(),
    )
    .unwrap()
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_write_all_read_into_roundtrip() {
    let dir = TempDir::new().unwrap();
    let atomic = create_mmap_resource(&dir, "test.bin");

    let data = b"hello atomic world";
    atomic.write_all(data).unwrap();

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(&buf, data);
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
#[case::failed(false)]
#[case::cancelled(true)]
fn mmap_unhealthy_resource_is_rejected_before_replacing_the_file(#[case] cancelled: bool) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("unhealthy.bin");
    let cancel = cancel_token();
    let atomic = Atomic::<MmapDriver>::open(
        cancel.clone(),
        MmapOptions::for_path(path.clone())
            .mode(OpenMode::ReadWrite)
            .build(),
    )
    .unwrap();
    atomic.write_all(b"original").unwrap();
    if cancelled {
        cancel.cancel();
    } else {
        atomic.read_settled(|inner| inner.fail_in_place("replace refused".to_owned()));
    }

    let result = atomic.write_all(b"replacement");

    if cancelled {
        assert!(matches!(result, Err(StorageError::Cancelled)));
    } else {
        assert!(matches!(result, Err(StorageError::Failed(reason)) if reason == "replace refused"));
    }
    assert_eq!(fs::read(path).unwrap(), b"original");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_tmp_file_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let atomic = create_mmap_resource(&dir, "index.bin");

    atomic.write_all(b"data").unwrap();

    let tmp_files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().to_str().is_some_and(|s| s.contains(".tmp.")))
        .collect();
    assert!(
        tmp_files.is_empty(),
        "tmp files should not remain: {tmp_files:?}"
    );
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn mem_write_all_read_into_roundtrip() {
    let atomic = create_mem_resource();

    let data = b"in-memory data";
    atomic.write_all(data).unwrap();

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(&buf, data);
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn mem_write_all_overwrites_committed_data() {
    let atomic = create_mem_resource();

    atomic.write_all(b"first").unwrap();
    atomic.write_all(b"second version").unwrap();

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, b"second version".len());
    assert_eq!(&buf, b"second version");
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_read_into_empty_returns_zero() {
    let dir = TempDir::new().unwrap();
    let atomic = create_mmap_resource(&dir, "empty.bin");

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_overwrite_atomically() {
    let dir = TempDir::new().unwrap();
    let atomic = create_mmap_resource(&dir, "overwrite.bin");

    atomic.write_all(b"first version").unwrap();
    atomic.write_all(b"second version - longer data").unwrap();

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, b"second version - longer data".len());
    assert_eq!(&buf, b"second version - longer data");
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_path_returns_inner_path() {
    let dir = TempDir::new().unwrap();
    let atomic = create_mmap_resource(&dir, "path_test.bin");
    let expected = dir.path().join("path_test.bin");

    assert_eq!(atomic.path(), Some(expected.as_path()));
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn mem_path_returns_none() {
    let atomic = create_mem_resource();

    assert!(atomic.path().is_none());
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(3)))]
fn mmap_writer_waits_for_a_reader_before_releasing_backing() {
    let dir = TempDir::new().unwrap();
    let atomic = Arc::new(create_mmap_resource(&dir, "handover.bin"));
    atomic.write_all(b"old payload").unwrap();
    let (reading, reading_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (writing, writing_rx) = mpsc::channel();
    let (written, written_rx) = mpsc::channel();

    let reader = {
        let atomic = Arc::clone(&atomic);
        thread::spawn(move || {
            atomic.read_settled(|inner| {
                reading.send(()).unwrap();
                release_rx.recv().unwrap();
                let mut bytes = Vec::new();
                inner.read_into(&mut bytes).unwrap();
                assert_eq!(bytes, b"old payload");
            });
        })
    };
    reading_rx.recv().unwrap();
    let writer = {
        let atomic = Arc::clone(&atomic);
        thread::spawn(move || {
            writing.send(()).unwrap();
            let result = atomic.write_all(b"new payload");
            written.send(result).unwrap();
        })
    };
    writing_rx.recv().unwrap();
    assert!(
        written_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "writer replaced backing while a read was active"
    );
    release.send(()).unwrap();
    reader.join().unwrap();
    written_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    writer.join().unwrap();

    let mut bytes = Vec::new();
    atomic.read_into(&mut bytes).unwrap();
    assert_eq!(bytes, b"new payload");
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(3)))]
fn concurrent_mmap_writers_publish_one_complete_payload() {
    let dir = TempDir::new().unwrap();
    let atomic = Arc::new(create_mmap_resource(&dir, "writers.bin"));
    let start = Arc::new(Barrier::new(3));
    let writes: Vec<_> = b"XY"
        .iter()
        .map(|&byte| {
            let atomic = Arc::clone(&atomic);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                atomic.write_all(&[byte; 1024])
            })
        })
        .collect();
    start.wait();
    for write in writes {
        write.join().unwrap().unwrap();
    }
    let mut bytes = Vec::new();
    atomic.read_into(&mut bytes).unwrap();
    assert!(bytes == [b'X'; 1024] || bytes == [b'Y'; 1024]);
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn failed_memory_write_releases_handover() {
    let pools = pools_with_budget(64);
    let atomic = Atomic::<MemDriver>::open(
        CancelToken::never(),
        MemOptions::builder().buffer(byte_buffer(&pools)).build(),
    )
    .unwrap();
    assert!(atomic.write_all(&[1; 1024]).is_err());
    atomic.write_all(b"ok").unwrap();
    let mut bytes = Vec::new();
    atomic.read_into(&mut bytes).unwrap();
    assert_eq!(bytes, b"ok");
}
