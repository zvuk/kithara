#![forbid(unsafe_code)]

//! Persistent coverage index for tracking downloaded byte ranges.
//!
//! [`CoverageIndex<R>`] is a global index stored in `_index/cov.bin`, keyed by
//! resource identifier (e.g. URL). It uses [`DashMap`] for concurrent in-memory
//! access and flushes atomically to disk via [`Atomic<R>`].
//!
//! [`DiskCoverage<R>`] implements the [`Coverage`] trait on top of a shared
//! `CoverageIndex<R>`, providing per-resource coverage tracking with automatic
//! flush on drop.

use std::{collections::HashMap, ops::Range, sync::Arc};

use dashmap::DashMap;
use kithara_storage::{Atomic, Coverage, MemCoverage, ResourceExt};
use rangemap::RangeSet;

/// On-disk format for the coverage index.
#[derive(serde::Serialize, serde::Deserialize)]
struct CoverageIndexFile {
    version: u32,
    entries: HashMap<String, CoverageData>,
}

/// Per-resource coverage data (serializable).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CoverageData {
    total_size: Option<u64>,
    ranges: RangeSet<u64>,
}

impl CoverageData {
    fn to_mem_coverage(&self) -> MemCoverage {
        let mut mc = self
            .total_size
            .map_or_else(MemCoverage::new, MemCoverage::with_total_size);
        for range in self.ranges.iter() {
            mc.mark(range.clone());
        }
        mc
    }

    fn from_mem_coverage(mc: &MemCoverage) -> Self {
        let mut ranges = RangeSet::new();
        if let Some(total) = mc.total_size() {
            // Reconstruct ranges from gaps.
            let gaps = mc.gaps();
            if gaps.is_empty() && total > 0 {
                // No gaps means fully covered.
                ranges.insert(0..total);
            } else {
                // Build covered ranges from gaps.
                let mut pos = 0u64;
                for gap in &gaps {
                    if pos < gap.start {
                        ranges.insert(pos..gap.start);
                    }
                    pos = gap.end;
                }
                if pos < total {
                    ranges.insert(pos..total);
                }
            }
        }
        Self {
            total_size: mc.total_size(),
            ranges,
        }
    }
}

/// Global coverage index backed by a single file.
///
/// Maintains an in-memory [`DashMap`] for concurrent access. Changes are
/// flushed to disk atomically via [`flush()`](Self::flush).
pub struct CoverageIndex<R: ResourceExt> {
    state: DashMap<String, CoverageData>,
    res: Atomic<R>,
}

impl<R: ResourceExt> CoverageIndex<R> {
    /// Create from a resource. Corrupt/missing data results in empty state.
    pub fn new(res: R) -> Self {
        let atomic = Atomic::new(res);
        let state = DashMap::new();

        // Best-effort load existing data.
        let mut buf = Vec::new();
        if let Ok(n) = atomic.read_into(&mut buf)
            && n > 0
            && let Ok((file, _)) = bincode::serde::decode_from_slice::<CoverageIndexFile, _>(
                &buf,
                bincode::config::legacy(),
            )
        {
            for (key, data) in file.entries {
                state.insert(key, data);
            }
        }

        Self { state, res: atomic }
    }

    /// In-memory lookup (no disk I/O).
    pub fn get(&self, key: &str) -> Option<MemCoverage> {
        self.state.get(key).map(|entry| entry.to_mem_coverage())
    }

    /// Update in-memory entry (no flush).
    pub fn update(&self, key: &str, coverage: &MemCoverage) {
        self.state
            .insert(key.to_string(), CoverageData::from_mem_coverage(coverage));
    }

    /// Remove entry from in-memory state.
    pub fn remove(&self, key: &str) {
        self.state.remove(key);
    }

    /// Flush all in-memory state to disk atomically.
    pub fn flush(&self) {
        let entries: HashMap<String, CoverageData> = self
            .state
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        let file = CoverageIndexFile {
            version: 1,
            entries,
        };

        if let Ok(bytes) = bincode::serde::encode_to_vec(&file, bincode::config::legacy()) {
            let _ = self.res.write_all(&bytes);
        }
    }
}

/// Persistent [`Coverage`] implementation backed by [`CoverageIndex<R>`].
///
/// Wraps an in-memory [`MemCoverage`] and syncs changes to the shared index.
/// Flushes automatically on drop.
pub struct DiskCoverage<R: ResourceExt> {
    inner: MemCoverage,
    key: String,
    index: Arc<CoverageIndex<R>>,
    dirty: bool,
}

impl<R: ResourceExt> DiskCoverage<R> {
    /// Open or create coverage for a resource.
    ///
    /// Loads existing coverage from `CoverageIndex` (in-memory lookup).
    pub fn open(index: Arc<CoverageIndex<R>>, key: String) -> Self {
        let inner = index.get(&key).unwrap_or_default();
        Self {
            inner,
            key,
            index,
            dirty: false,
        }
    }

    /// Sync in-memory state to the shared index and flush to disk.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.index.update(&self.key, &self.inner);
        self.index.flush();
        self.dirty = false;
    }

    /// Remove this entry from the index (e.g. resource fully downloaded).
    pub fn remove(mut self) {
        self.dirty = false; // Prevent flush in Drop.
        self.index.remove(&self.key);
        self.index.flush();
    }
}

impl<R: ResourceExt> Coverage for DiskCoverage<R> {
    fn mark(&mut self, range: Range<u64>) {
        self.inner.mark(range);
        self.dirty = true;
    }

    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }

    fn next_gap(&self, max_size: u64) -> Option<Range<u64>> {
        self.inner.next_gap(max_size)
    }

    fn gaps(&self) -> Vec<Range<u64>> {
        self.inner.gaps()
    }

    fn total_size(&self) -> Option<u64> {
        self.inner.total_size()
    }

    fn set_total_size(&mut self, size: u64) {
        self.inner.set_total_size(size);
        self.dirty = true;
    }
}

impl<R: ResourceExt> Drop for DiskCoverage<R> {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
    use rstest::rstest;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

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
        .unwrap()
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn open_empty_file_returns_empty_state() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir, "cov.bin");
        let index = CoverageIndex::new(res);

        assert!(index.get("nonexistent").is_none());
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn get_nonexistent_key_returns_none() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir, "cov.bin");
        let index = CoverageIndex::new(res);

        assert!(index.get("missing-key").is_none());
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn corrupt_file_returns_empty_state() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir, "cov.bin");

        // Write corrupt data.
        res.write_all(b"not valid bincode").unwrap();

        let index = CoverageIndex::new(res);
        assert!(index.get("anything").is_none());
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn concurrent_updates_from_different_threads() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir, "cov.bin");
        let index = Arc::new(CoverageIndex::new(res));

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let idx = Arc::clone(&index);
                std::thread::spawn(move || {
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn disk_coverage_open_without_existing_entry() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir, "cov.bin");
        let index = Arc::new(CoverageIndex::new(res));

        let cov = DiskCoverage::open(Arc::clone(&index), "new-key".to_string());
        assert!(!cov.is_complete());
        assert!(cov.total_size().is_none());
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
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
}
