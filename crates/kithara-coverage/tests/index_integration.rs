#![cfg(not(target_arch = "wasm32"))]

use std::{sync::Arc, time::Duration};

use kithara_coverage::{Coverage, CoverageIndex, DiskCoverage, MemCoverage};
use kithara_platform::thread;
use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceExt};
use kithara_test_utils::kithara;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn create_test_resource(dir: &TempDir, name: &str) -> MmapResource {
    let path = dir.path().join(name);
    Resource::open(
        CancellationToken::new(),
        MmapOptions {
            path,
            initial_len: Some(4096),
            mode: OpenMode::ReadWrite,
        },
    )
    .expect("failed to open mmap coverage resource")
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn open_empty_file_returns_empty_state() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = CoverageIndex::new(res);

    assert!(index.get("nonexistent").is_none());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn get_nonexistent_key_returns_none() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = CoverageIndex::new(res);

    assert!(index.get("missing-key").is_none());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn update_and_get_roundtrip() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = CoverageIndex::new(res);

    let mut mc = MemCoverage::with_total_size(1000);
    mc.mark(0..500);

    index.update("key1", &mc);

    let loaded = index.get("key1").unwrap();
    assert_eq!(loaded.total_size(), Some(1000));
    assert!(!loaded.is_complete());
    let gaps = loaded.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0], 500..1000);
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn update_two_different_keys() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = CoverageIndex::new(res);

    let mut mc1 = MemCoverage::with_total_size(100);
    mc1.mark(0..50);
    index.update("key1", &mc1);

    let mut mc2 = MemCoverage::with_total_size(200);
    mc2.mark(0..200);
    index.update("key2", &mc2);

    let loaded1 = index.get("key1").unwrap();
    assert!(!loaded1.is_complete());

    let loaded2 = index.get("key2").unwrap();
    assert!(loaded2.is_complete());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn remove_deletes_entry() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = CoverageIndex::new(res);

    let mc = MemCoverage::with_total_size(100);
    index.update("key1", &mc);
    assert!(index.get("key1").is_some());

    index.remove("key1");
    assert!(index.get("key1").is_none());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn flush_and_reopen_persists() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cov.bin");

    // First instance: write and flush.
    {
        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path: path.clone(),
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let index = CoverageIndex::new(res);

        let mut mc = MemCoverage::with_total_size(1000);
        mc.mark(0..500);
        index.update("persisted-key", &mc);
        index.flush();
    }

    // Second instance: reopen and verify.
    {
        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let index = CoverageIndex::new(res);

        let loaded = index.get("persisted-key").unwrap();
        assert_eq!(loaded.total_size(), Some(1000));
        assert!(!loaded.is_complete());
        let gaps = loaded.gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0], 500..1000);
    }
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn corrupt_file_returns_empty_state() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");

    // Write corrupt data.
    res.write_all(b"not valid bincode").unwrap();

    let index = CoverageIndex::new(res);
    assert!(index.get("anything").is_none());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn concurrent_updates_from_different_threads() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = Arc::new(CoverageIndex::new(res));

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let idx = Arc::clone(&index);
            thread::spawn(move || {
                let key = format!("thread-key-{i}");
                let mut mc = MemCoverage::with_total_size(100);
                mc.mark(0..50);
                idx.update(&key, &mc);
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    for i in 0..4 {
        let key = format!("thread-key-{i}");
        assert!(index.get(&key).is_some(), "key {key} should exist");
    }
}

// DiskCoverage tests

#[kithara::test(timeout(Duration::from_secs(2)))]
fn disk_coverage_open_without_existing_entry() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = Arc::new(CoverageIndex::new(res));

    let cov = DiskCoverage::open(Arc::clone(&index), "new-key".to_string());
    assert!(!cov.is_complete());
    assert!(cov.total_size().is_none());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn disk_coverage_mark_flush_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cov.bin");

    let res: MmapResource = Resource::open(
        CancellationToken::new(),
        MmapOptions {
            path: path.clone(),
            initial_len: Some(4096),
            mode: OpenMode::ReadWrite,
        },
    )
    .unwrap();
    let index = Arc::new(CoverageIndex::new(res));

    // Write coverage and flush.
    {
        let mut cov = DiskCoverage::open(Arc::clone(&index), "test-key".to_string());
        cov.set_total_size(1000);
        cov.mark(0..500);
        cov.flush();
    }

    // Reopen from same index (in-memory).
    {
        let cov = DiskCoverage::open(Arc::clone(&index), "test-key".to_string());
        assert_eq!(cov.total_size(), Some(1000));
        assert!(!cov.is_complete());
        let gaps = cov.gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0], 500..1000);
    }

    // Reopen from disk (new index).
    {
        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let index2 = Arc::new(CoverageIndex::new(res));
        let cov = DiskCoverage::open(index2, "test-key".to_string());
        assert_eq!(cov.total_size(), Some(1000));
        assert!(!cov.is_complete());
    }
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn disk_coverage_flush_idempotent() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = Arc::new(CoverageIndex::new(res));

    let mut cov = DiskCoverage::open(Arc::clone(&index), "key".to_string());
    cov.set_total_size(100);
    cov.mark(0..50);
    cov.flush();

    // Second flush without changes — no-op.
    cov.flush();
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn disk_coverage_remove_deletes_from_index() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = Arc::new(CoverageIndex::new(res));

    let mut cov = DiskCoverage::open(Arc::clone(&index), "to-remove".to_string());
    cov.set_total_size(100);
    cov.mark(0..100);
    cov.flush();

    assert!(index.get("to-remove").is_some());

    // Remove — should delete from index.
    cov.remove();
    assert!(index.get("to-remove").is_none());
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn disk_coverage_drop_flushes() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = Arc::new(CoverageIndex::new(res));

    {
        let mut cov = DiskCoverage::open(Arc::clone(&index), "drop-key".to_string());
        cov.set_total_size(200);
        cov.mark(0..100);
        // Drop without explicit flush.
    }

    // Should have been flushed on drop.
    let loaded = index.get("drop-key").unwrap();
    assert_eq!(loaded.total_size(), Some(200));
}

#[kithara::test(timeout(Duration::from_secs(2)))]
fn multiple_disk_coverages_independent() {
    let dir = TempDir::new().unwrap();
    let res = create_test_resource(&dir, "cov.bin");
    let index = Arc::new(CoverageIndex::new(res));

    {
        let mut cov1 = DiskCoverage::open(Arc::clone(&index), "key-a".to_string());
        cov1.set_total_size(100);
        cov1.mark(0..50);
        cov1.flush();
    }

    {
        let mut cov2 = DiskCoverage::open(Arc::clone(&index), "key-b".to_string());
        cov2.set_total_size(200);
        cov2.mark(0..200);
        cov2.flush();
    }

    let a = index.get("key-a").unwrap();
    assert!(!a.is_complete());

    let b = index.get("key-b").unwrap();
    assert!(b.is_complete());
}
