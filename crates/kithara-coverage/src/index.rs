#![forbid(unsafe_code)]

//! Persistent coverage index for tracking downloaded byte ranges.
//!
//! [`CoverageIndex<R>`] is a global index stored in `_index/cov.bin`, keyed by
//! resource identifier (e.g. URL). It uses [`kithara_platform::RwLock`] for
//! concurrent in-memory access and flushes atomically to disk via [`Atomic<R>`].
//!
//! [`DiskCoverage<R>`] implements the [`Coverage`] trait on top of a shared
//! `CoverageIndex<R>`, providing per-resource coverage tracking with automatic
//! flush on drop.

use std::{collections::HashMap, ops::Range, sync::Arc};

use kithara_platform::RwLock;
use kithara_storage::{Atomic, ResourceExt};
use rangemap::RangeSet;

use crate::{Coverage, MemCoverage};

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
/// Maintains an in-memory [`HashMap`] behind a [`RwLock`] for concurrent
/// access. Uses `kithara_platform::RwLock` which is WASM-safe (spin-loop
/// instead of `Atomics.wait` on the browser main thread). Changes are flushed
/// to disk atomically via [`flush()`](Self::flush).
pub struct CoverageIndex<R: ResourceExt> {
    state: RwLock<HashMap<String, CoverageData>>,
    res: Atomic<R>,
}

impl<R: ResourceExt> CoverageIndex<R> {
    /// Create from a resource. Corrupt/missing data results in empty state.
    pub fn new(res: R) -> Self {
        let atomic = Atomic::new(res);
        let mut state = HashMap::new();

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

        Self {
            state: RwLock::new(state),
            res: atomic,
        }
    }

    /// In-memory lookup (no disk I/O).
    pub fn get(&self, key: &str) -> Option<MemCoverage> {
        self.state
            .read()
            .get(key)
            .map(CoverageData::to_mem_coverage)
    }

    /// Update in-memory entry (no flush).
    pub fn update(&self, key: &str, coverage: &MemCoverage) {
        self.state
            .write()
            .insert(key.to_string(), CoverageData::from_mem_coverage(coverage));
    }

    /// Remove entry from in-memory state.
    pub fn remove(&self, key: &str) {
        self.state.write().remove(key);
    }

    /// Flush all in-memory state to disk atomically.
    pub fn flush(&self) {
        let entries: HashMap<String, CoverageData> = self.state.read().clone();

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
