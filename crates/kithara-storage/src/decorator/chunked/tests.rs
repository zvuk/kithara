#![forbid(unsafe_code)]
#![cfg(test)]
#![cfg(not(target_arch = "wasm32"))]

//! Unit tests for the chunked-atomic decorator. Lives in its own file
//! per the workspace `lib.rs` / `mod.rs` discipline rule.

use std::{
    fs,
    path::{Path, PathBuf},
};

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use kithara_platform::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::core::{AtomicChunked, OpenIntent, make_tmp_path};
use crate::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt};

fn open_chunked(dir: &TempDir, name: &str) -> (AtomicChunked<MmapResource>, PathBuf, PathBuf) {
    let canonical = dir.path().join(name);
    let cancel = CancellationToken::new();
    let res = AtomicChunked::<MmapResource>::open(canonical.clone(), move |target, intent| {
        let mode = match intent {
            OpenIntent::Fresh => OpenMode::ReadWrite,
            OpenIntent::Reopen => OpenMode::ReadOnly,
        };
        Resource::open(
            cancel.clone(),
            MmapOptions {
                mode,
                initial_len: None,
                path: target.to_path_buf(),
            },
        )
    })
    .unwrap();
    let tmp = make_tmp_path(&canonical).unwrap();
    (res, canonical, tmp)
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn canonical_invisible_until_commit() {
    let dir = TempDir::new().unwrap();
    let (res, canonical, tmp) = open_chunked(&dir, "seg.bin");

    res.write_at(0, b"chunk-1-").unwrap();
    res.write_at(8, b"chunk-2!").unwrap();

    assert!(
        !canonical.exists(),
        "canonical must not exist before commit"
    );
    assert!(tmp.exists(), "tmp file must hold in-flight bytes");

    res.commit(Some(16)).unwrap();
    assert!(canonical.exists(), "canonical materialised on commit");
    assert!(!tmp.exists(), "tmp consumed by atomic rename");

    let bytes = fs::read(&canonical).unwrap();
    assert_eq!(&bytes, b"chunk-1-chunk-2!");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn drop_without_commit_cleans_tmp() {
    let dir = TempDir::new().unwrap();
    let (res, canonical, tmp) = open_chunked(&dir, "abandoned.bin");

    res.write_at(0, b"will-not-commit").unwrap();
    assert!(tmp.exists());
    drop(res);

    assert!(!tmp.exists(), "Drop must remove the orphaned tmp");
    assert!(!canonical.exists(), "canonical must never appear");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn fail_cleans_tmp() {
    let dir = TempDir::new().unwrap();
    let (res, canonical, tmp) = open_chunked(&dir, "failed.bin");

    res.write_at(0, b"oops").unwrap();
    res.fail("test".into());

    assert!(!tmp.exists(), "fail() must remove the tmp");
    assert!(!canonical.exists());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn open_wipes_stale_tmp_from_previous_run() {
    let dir = TempDir::new().unwrap();
    let canonical = dir.path().join("survivor.bin");
    let stale_tmp = make_tmp_path(&canonical).unwrap();
    // Simulate a crashed previous writer that left orphan bytes.
    fs::write(&stale_tmp, b"stale-from-previous-process").unwrap();

    let cancel = CancellationToken::new();
    let res = AtomicChunked::<MmapResource>::open(canonical.clone(), move |target, intent| {
        let mode = match intent {
            OpenIntent::Fresh => OpenMode::ReadWrite,
            OpenIntent::Reopen => OpenMode::ReadOnly,
        };
        Resource::open(
            cancel.clone(),
            MmapOptions {
                mode,
                initial_len: None,
                path: target.to_path_buf(),
            },
        )
    })
    .unwrap();
    res.write_at(0, b"fresh").unwrap();
    res.commit(Some(5)).unwrap();
    let bytes = fs::read(&canonical).unwrap();
    assert_eq!(&bytes, b"fresh");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn read_after_commit_returns_payload_via_decorator() {
    // Smoke: write chunks, commit, then read back THROUGH the
    // decorator (not direct fs::read). Pins that the inner mmap
    // remains usable after the rename — reads must surface the
    // committed payload exactly.
    let dir = TempDir::new().unwrap();
    let (res, _, _) = open_chunked(&dir, "post-commit-read.bin");
    res.write_at(0, b"chunk-1-").unwrap();
    res.write_at(8, b"chunk-2!").unwrap();
    res.commit(Some(16)).unwrap();

    let mut buf = [0u8; 16];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 16, "post-commit read must return all bytes");
    assert_eq!(&buf, b"chunk-1-chunk-2!");

    // Read of the tail (last byte) — exact same condition Apple's
    // probe hits in the failing integration test.
    let mut tail = [0u8; 1];
    let n = res.read_at(15, &mut tail).unwrap();
    assert_eq!(n, 1);
    assert_eq!(tail[0], b'!');
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn read_during_writes_observes_inner_state() {
    let dir = TempDir::new().unwrap();
    let (res, _, _) = open_chunked(&dir, "live.bin");
    res.write_at(0, b"live-bytes").unwrap();
    let mut buf = [0u8; 10];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 10);
    assert_eq!(&buf, b"live-bytes");
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn passthrough_for_memory_inner_has_no_tmp() {
    // For mem-backed inners (no filesystem), the decorator
    // delegates straight through and never creates a tmp file.
    let mem = crate::MemResource::new(CancellationToken::new());
    let res = AtomicChunked::passthrough(mem, PathBuf::from("virtual"));
    res.write_at(0, b"in-mem").unwrap();
    res.commit(Some(6)).unwrap();
    let mut buf = [0u8; 6];
    res.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, b"in-mem");
    // path() reports canonical; nothing on disk.
    assert_eq!(res.path(), Some(Path::new("virtual")));
}
