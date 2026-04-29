#![forbid(unsafe_code)]
#![cfg(test)]

//! Unit tests for the whole-file atomic decorator. Lives in its own
//! file per the workspace `lib.rs` / `mod.rs` discipline rule.

#[cfg(not(target_arch = "wasm32"))]
use std::fs;

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use kithara_platform::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::core::Atomic;
use crate::{MemOptions, MemResource, Resource, ResourceExt};
#[cfg(not(target_arch = "wasm32"))]
use crate::{MmapOptions, MmapResource, OpenMode};

#[cfg(not(target_arch = "wasm32"))]
fn create_mmap_resource(dir: &TempDir, name: &str) -> MmapResource {
    let path = dir.path().join(name);
    Resource::open(
        CancellationToken::new(),
        MmapOptions {
            path,
            mode: OpenMode::ReadWrite,
            initial_len: Some(4096),
        },
    )
    .unwrap()
}

fn create_mem_resource() -> MemResource {
    Resource::open(CancellationToken::new(), MemOptions::default()).unwrap()
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_write_all_read_into_roundtrip() {
    let dir = TempDir::new().unwrap();
    let res = create_mmap_resource(&dir, "test.bin");
    let atomic = Atomic::new(res);

    let data = b"hello atomic world";
    atomic.write_all(data).unwrap();

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(&buf, data);
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_tmp_file_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let res = create_mmap_resource(&dir, "index.bin");
    let atomic = Atomic::new(res);

    atomic.write_all(b"data").unwrap();

    // No tmp files should remain (they have unique suffixes like index.tmp.0).
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
    let res = create_mem_resource();
    let atomic = Atomic::new(res);

    let data = b"in-memory data";
    atomic.write_all(data).unwrap();

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(&buf, data);
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn mem_write_all_overwrites_committed_data() {
    let res = create_mem_resource();
    let atomic = Atomic::new(res);

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
    let res = create_mmap_resource(&dir, "empty.bin");
    let atomic = Atomic::new(res);

    let mut buf = Vec::new();
    let n = atomic.read_into(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(timeout(Duration::from_secs(2)))]
fn mmap_overwrite_atomically() {
    let dir = TempDir::new().unwrap();
    let res = create_mmap_resource(&dir, "overwrite.bin");
    let atomic = Atomic::new(res);

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
    let res = create_mmap_resource(&dir, "path_test.bin");
    let expected = dir.path().join("path_test.bin");
    let atomic = Atomic::new(res);

    assert_eq!(atomic.path(), Some(expected.as_path()));
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn mem_path_returns_none() {
    let res = create_mem_resource();
    let atomic = Atomic::new(res);

    assert!(atomic.path().is_none());
}
