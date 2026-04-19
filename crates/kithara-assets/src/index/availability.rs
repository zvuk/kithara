#![forbid(unsafe_code)]

//! Per-resource byte availability index.
//!
//! `Availability` is a single resource's snapshot of which byte ranges
//! have been written and whether it has been committed.

use std::{collections::BTreeMap, ops::Range, sync::Arc};

use dashmap::DashMap;
use kithara_platform::Mutex;
use kithara_storage::{Atomic, AvailabilityObserver, ResourceExt, StorageError};
use rangemap::RangeSet;
use rkyv::option::ArchivedOption;

use super::schema::{AssetAvailabilityFile, AvailabilityFile, ResourceAvailabilityFile};
use crate::{
    error::{AssetsError, AssetsResult},
    key::ResourceKey,
};

/// Byte-level availability state for a single resource.
#[derive(Clone, Debug, Default)]
pub(crate) struct Availability {
    pub(crate) ranges: RangeSet<u64>,
    pub(crate) final_len: Option<u64>,
    pub(crate) committed: bool,
}

impl Availability {
    fn insert(&mut self, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        self.ranges.insert(range);
    }

    fn mark_committed(&mut self, final_len: u64) {
        self.committed = true;
        self.final_len = Some(final_len);
        if final_len > 0 {
            self.ranges.insert(0..final_len);
        }
    }

    fn contains(&self, range: &Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        self.ranges.gaps(range).next().is_none()
    }
}

/// Opaque handle to the aggregate byte availability index.
#[derive(Clone)]
pub struct AvailabilityIndex {
    inner: Arc<InnerIndex>,
}

type AssetMap = DashMap<String, Arc<DashMap<String, Arc<Mutex<Availability>>>>>;

struct InnerIndex {
    /// Maps `asset_root` -> `RelativePath` -> `Availability`
    assets: AssetMap,
}

impl AvailabilityIndex {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(InnerIndex {
                assets: DashMap::new(),
            }),
        }
    }

    pub(crate) fn record_write(&self, asset_root: &str, key: &ResourceKey, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        let (root, path) = Self::resolve_refs(asset_root, key);
        let arc = self.insert_or_get_entry(root, path);
        arc.lock_sync().insert(range);
    }

    pub(crate) fn record_commit(&self, asset_root: &str, key: &ResourceKey, final_len: u64) {
        let (root, path) = Self::resolve_refs(asset_root, key);
        let arc = self.insert_or_get_entry(root, path);
        arc.lock_sync().mark_committed(final_len);
    }

    pub(crate) fn available_ranges(&self, asset_root: &str, key: &ResourceKey) -> RangeSet<u64> {
        let (root, path) = Self::resolve_refs(asset_root, key);
        if let Some(asset) = self.inner.assets.get(root)
            && let Some(arc) = asset.get(path)
        {
            return arc.lock_sync().ranges.clone();
        }
        RangeSet::new()
    }

    pub(crate) fn contains_range(
        &self,
        asset_root: &str,
        key: &ResourceKey,
        range: Range<u64>,
    ) -> bool {
        if range.start >= range.end {
            return true;
        }
        let (root, path) = Self::resolve_refs(asset_root, key);
        if let Some(asset) = self.inner.assets.get(root)
            && let Some(arc) = asset.get(path)
        {
            return arc.lock_sync().contains(&range);
        }
        false
    }

    pub(crate) fn final_len(&self, asset_root: &str, key: &ResourceKey) -> Option<u64> {
        let (root, path) = Self::resolve_refs(asset_root, key);
        if let Some(asset) = self.inner.assets.get(root)
            && let Some(arc) = asset.get(path)
        {
            return arc.lock_sync().final_len;
        }
        None
    }

    pub(crate) fn remove(&self, asset_root: &str, key: &ResourceKey) {
        let (root, path) = Self::resolve_refs(asset_root, key);
        if let Some(asset) = self.inner.assets.get(root) {
            asset.remove(path);
        }
    }

    fn resolve_refs<'a>(asset_root: &'a str, key: &'a ResourceKey) -> (&'a str, &'a str) {
        match key {
            ResourceKey::Relative(path) => (asset_root, path.as_str()),
            ResourceKey::Absolute(path) => ("__absolute__", path.to_str().unwrap_or("")),
        }
    }

    fn insert_or_get_entry(&self, asset_root: &str, path: &str) -> Arc<Mutex<Availability>> {
        let asset_map = self.inner.assets.get(asset_root).map_or_else(
            || {
                self.inner
                    .assets
                    .entry(asset_root.to_string())
                    .or_insert_with(|| Arc::new(DashMap::new()))
                    .clone()
            },
            |map| map.clone(),
        );

        asset_map.get(path).map_or_else(
            || {
                asset_map
                    .entry(path.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(Availability::default())))
                    .clone()
            },
            |arc| arc.clone(),
        )
    }

    /// Load the availability index from a persistent resource.
    pub(crate) fn load_from<R: ResourceExt>(&self, res: &Atomic<R>) -> AssetsResult<()> {
        let mut buf = Vec::new();
        let n = res.read_into(&mut buf)?;
        if n == 0 {
            return Ok(());
        }

        let archived =
            match rkyv::access::<super::schema::ArchivedAvailabilityFile, rkyv::rancor::Error>(
                &buf[..n],
            ) {
                Ok(archived) => archived,
                Err(e) => {
                    tracing::debug!("Failed to validate availability index: {}", e);
                    return Ok(());
                }
            };

        for (root, asset_record) in archived.assets.iter() {
            let root_str = root.as_str().to_string();
            let asset_map = Arc::new(DashMap::new());

            for (path, res_record) in asset_record.resources.iter() {
                let mut avail = Availability::default();
                for r in res_record.ranges.iter() {
                    let start = r.0.to_native();
                    let end = r.1.to_native();
                    avail.insert(start..end);
                }

                let final_len: Option<u64> = match res_record.final_len {
                    ArchivedOption::Some(ref l) => Some(l.to_native()),
                    ArchivedOption::None => None,
                };

                if let Some(flen) = final_len {
                    avail.mark_committed(flen);
                } else {
                    avail.committed = res_record.committed;
                }

                asset_map.insert(path.as_str().to_string(), Arc::new(Mutex::new(avail)));
            }

            self.inner.assets.insert(root_str, asset_map);
        }
        Ok(())
    }

    /// Persist the aggregate index to storage atomically.
    pub(crate) fn persist_to<R: ResourceExt>(&self, res: &Atomic<R>) -> AssetsResult<()> {
        let mut availability_file = AvailabilityFile {
            version: 1,
            assets: BTreeMap::new(),
        };

        // Dump memory state into the file representation
        for entry in &self.inner.assets {
            let root = entry.key();
            let memory_asset = entry.value();

            let disk_asset = availability_file
                .assets
                .entry(root.clone())
                .or_insert_with(|| AssetAvailabilityFile {
                    resources: BTreeMap::new(),
                });

            for res_entry in &**memory_asset {
                let path = res_entry.key();
                let avail = res_entry.value().lock_sync();

                let ranges = avail.ranges.iter().map(|r| (r.start, r.end)).collect();

                disk_asset.resources.insert(
                    path.clone(),
                    ResourceAvailabilityFile {
                        ranges,
                        final_len: avail.final_len,
                        committed: avail.committed,
                    },
                );
            }
        }

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&availability_file)
            .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
        res.write_all(&bytes)?;

        Ok(())
    }
}

impl Default for AvailabilityIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AvailabilityIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvailabilityIndex")
            .field("tracked_assets", &self.inner.assets.len())
            .finish()
    }
}

/// `kithara_storage::AvailabilityObserver` implementation scoped to a
/// single `ResourceKey`.
pub(crate) struct ScopedAvailabilityObserver {
    asset_root: String,
    key: ResourceKey,
    index: AvailabilityIndex,
    /// Optional signal shared with [`crate::disk_store::DiskAssetStore`]'s
    /// auto-flush background task. `None` disables the commit-debounce
    /// trigger (historical behaviour: callers drive `checkpoint()`
    /// explicitly).
    #[cfg(not(target_arch = "wasm32"))]
    signal: Option<Arc<super::super::disk_store::CheckpointSignal>>,
}

impl ScopedAvailabilityObserver {
    pub(crate) fn new(
        asset_root: String,
        key: ResourceKey,
        index: AvailabilityIndex,
        #[cfg(not(target_arch = "wasm32"))] signal: Option<
            Arc<super::super::disk_store::CheckpointSignal>,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            asset_root,
            key,
            index,
            #[cfg(not(target_arch = "wasm32"))]
            signal,
        })
    }
}

impl AvailabilityObserver for ScopedAvailabilityObserver {
    fn on_write(&self, range: Range<u64>) {
        self.index.record_write(&self.asset_root, &self.key, range);
    }

    fn on_commit(&self, final_len: u64) {
        self.index
            .record_commit(&self.asset_root, &self.key, final_len);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(ref signal) = self.signal {
            signal.on_commit();
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::time::Duration;

    use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_default_is_empty() {
        let a = Availability::default();
        assert!(a.ranges.is_empty());
        assert!(a.final_len.is_none());
        assert!(!a.committed);
        assert!(a.contains(&(0..0)));
        assert!(!a.contains(&(0..1)));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_insert_then_contains() {
        let mut a = Availability::default();
        a.insert(0..100);
        assert!(a.contains(&(0..100)));
        assert!(a.contains(&(10..90)));
        assert!(!a.contains(&(50..150)));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_insert_empty_is_noop() {
        let mut a = Availability::default();
        a.insert(5..5);
        assert!(a.ranges.is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_contains_rejects_gaps() {
        let mut a = Availability::default();
        a.insert(0..10);
        a.insert(20..30);
        assert!(a.contains(&(0..10)));
        assert!(a.contains(&(20..30)));
        assert!(!a.contains(&(0..20)));
        assert!(!a.contains(&(5..25)));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_mark_committed_covers_full_range() {
        let mut a = Availability::default();
        a.mark_committed(10);
        assert!(a.committed);
        assert_eq!(a.final_len, Some(10));
        assert!(a.contains(&(0..10)));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_mark_committed_zero_len_has_no_range() {
        let mut a = Availability::default();
        a.mark_committed(0);
        assert!(a.committed);
        assert_eq!(a.final_len, Some(0));
        assert!(a.ranges.is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_per_key_isolation() {
        let idx = AvailabilityIndex::new();
        let k1 = ResourceKey::new("file1");
        let k2 = ResourceKey::new("file2");

        idx.record_write("test_asset", &k1, 0..10);
        idx.record_write("test_asset", &k2, 20..30);

        assert!(idx.contains_range("test_asset", &k1, 0..10));
        assert!(!idx.contains_range("test_asset", &k1, 20..30));
        assert!(idx.contains_range("test_asset", &k2, 20..30));
        assert!(!idx.contains_range("test_asset", &k2, 0..10));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_new_is_empty() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::new("file1");
        assert!(!idx.contains_range("test_asset", &k, 0..10));
        assert_eq!(idx.final_len("test_asset", &k), None);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_commit_sets_final_len_and_full_range() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::new("file1");

        idx.record_commit("test_asset", &k, 50);

        assert_eq!(idx.final_len("test_asset", &k), Some(50));
        assert!(idx.contains_range("test_asset", &k, 0..50));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_write_slow_then_fast_path() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::new("file1");

        idx.record_write("test_asset", &k, 0..10);
        assert!(idx.contains_range("test_asset", &k, 0..10));

        idx.record_write("test_asset", &k, 10..20);
        assert!(idx.contains_range("test_asset", &k, 0..20));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_write_empty_range_is_noop() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::new("file1");

        idx.record_write("test_asset", &k, 10..10);
        assert!(!idx.contains_range("test_asset", &k, 10..11));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_remove_clears_entry() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::new("file1");

        idx.record_write("test_asset", &k, 0..10);
        idx.remove("test_asset", &k);

        assert!(!idx.contains_range("test_asset", &k, 0..10));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_snapshot_and_seed_roundtrip() {
        let dir = TempDir::new().unwrap();
        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path: dir.path().join("availability.bin"),
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let atomic = Atomic::new(res);

        let idx1 = AvailabilityIndex::new();
        let k1 = ResourceKey::new("file1");
        let k2 = ResourceKey::new("file2");

        idx1.record_write("test_asset", &k1, 0..10);
        idx1.record_commit("test_asset", &k2, 50);

        idx1.persist_to(&atomic).unwrap();

        let idx2 = AvailabilityIndex::new();
        idx2.load_from(&atomic).unwrap();

        assert!(idx2.contains_range("test_asset", &k1, 0..10));
        assert_eq!(idx2.final_len("test_asset", &k2), Some(50));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_empty_resource_loads_empty() {
        let dir = TempDir::new().unwrap();
        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path: dir.path().join("availability.bin"),
                initial_len: None,
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let atomic = Atomic::new(res);

        let idx = AvailabilityIndex::new();
        idx.load_from(&atomic).unwrap();
        assert!(idx.inner.assets.is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_corrupt_payload_loads_empty() {
        let dir = TempDir::new().unwrap();
        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path: dir.path().join("availability.bin"),
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let atomic = Atomic::new(res);
        atomic.write_all(b"not valid bytes").unwrap();

        let idx = AvailabilityIndex::new();
        idx.load_from(&atomic).unwrap();
        assert!(idx.inner.assets.is_empty());
    }
}
