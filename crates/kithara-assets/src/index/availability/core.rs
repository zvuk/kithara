#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Weak;
use std::{
    ops::Range,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use dashmap::DashMap;
use kithara_platform::Mutex;
use kithara_storage::AvailabilityObserver;
use rangemap::RangeSet;

use crate::{
    error::AssetsResult,
    flush::{FlushHub, Flushable},
    key::ResourceKey,
};

/// Byte-level availability state for a single resource.
#[derive(Clone, Debug, Default)]
pub(crate) struct Availability {
    pub(crate) final_len: Option<u64>,
    pub(crate) ranges: RangeSet<u64>,
    pub(crate) committed: bool,
}

impl Availability {
    fn contains(&self, range: &Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        self.ranges.gaps(range).next().is_none()
    }

    pub(super) fn insert(&mut self, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        self.ranges.insert(range);
    }

    pub(super) fn mark_committed(&mut self, final_len: u64) {
        self.committed = true;
        self.final_len = Some(final_len);
        if final_len > 0 {
            self.ranges.insert(0..final_len);
        }
    }
}

/// Opaque handle to the aggregate byte availability index.
#[derive(Clone)]
pub struct AvailabilityIndex {
    pub(super) inner: Arc<InnerIndex>,
}

pub(super) type AssetMap = DashMap<String, Arc<DashMap<String, Arc<Mutex<Availability>>>>>;

pub(super) struct InnerIndex {
    /// Maps `asset_root` -> `RelativePath` -> `Availability`
    pub(super) assets: AssetMap,
    /// `true` when the in-memory aggregate has uncommitted writes
    /// since the last successful flush.
    pub(super) dirty: AtomicBool,
    /// Set by [`AvailabilityIndex::attach_to`]. While `None`,
    /// `ScopedAvailabilityObserver` falls back to the legacy
    /// "explicit checkpoint only" contract — every observer event
    /// just marks `dirty` so the next call to
    /// [`AvailabilityIndex::flush`] writes the snapshot.
    pub(super) hub: OnceLock<Arc<FlushHub>>,
    /// Disk-backed persist target. Set once via
    /// `AvailabilityIndex::enable_persistence`; later flushes reuse
    /// the cached `Atomic<MmapDriver>` handle. Native only.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) persist: OnceLock<super::disk::AvailabilityPersist>,
}

impl AvailabilityIndex {
    // ast-grep-ignore: style.prefer-default-derive
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(InnerIndex {
                assets: DashMap::new(),
                #[cfg(not(target_arch = "wasm32"))]
                persist: OnceLock::new(),
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
            }),
        }
    }

    /// Bind this aggregate to a [`FlushHub`] for coordinated flushing.
    /// Called once per `AssetStore` build; subsequent calls are no-ops.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        if self.inner.hub.set(Arc::clone(hub)).is_err() {
            return;
        }
        hub.register(Arc::downgrade(&self.inner) as Weak<dyn Flushable>);
    }

    pub(crate) fn available_ranges(&self, key: &ResourceKey) -> RangeSet<u64> {
        let (root, path) = Self::resolve_refs(key);
        if let Some(asset) = self.inner.assets.get(root)
            && let Some(arc) = asset.get(path)
        {
            return arc.lock().ranges.clone();
        }
        RangeSet::new()
    }

    /// Drop every per-resource entry recorded under `asset_root`.
    ///
    /// Used by deletion paths that wipe an entire asset directory at
    /// once (`DiskAssetStore::delete_asset`, `MemAssetStore::delete_asset`,
    /// the LRU evictor's `delete_asset_dir`). Without this, stale
    /// `final_len` / `ranges` survive on the index map and
    /// `contains_range` answers `true` for bytes that no longer exist
    /// on disk — producing the HLS hang pinned by
    /// `red_test_delete_asset_strands_availability_index`.
    pub(crate) fn clear_root(&self, asset_root: &str) {
        self.inner.assets.remove(asset_root);
    }

    pub(crate) fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        let (root, path) = Self::resolve_refs(key);
        if let Some(asset) = self.inner.assets.get(root)
            && let Some(arc) = asset.get(path)
        {
            return arc.lock().contains(&range);
        }
        false
    }

    pub(crate) fn final_len(&self, key: &ResourceKey) -> Option<u64> {
        let (root, path) = Self::resolve_refs(key);
        if let Some(asset) = self.inner.assets.get(root)
            && let Some(arc) = asset.get(path)
        {
            return arc.lock().final_len;
        }
        None
    }

    /// Force a synchronous flush. Routes through [`FlushHub::flush_now`]
    /// when a hub is attached, or runs the inline serialise+write path
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Propagates the first per-source flush error encountered.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn flush(&self) -> AssetsResult<()> {
        self.inner
            .hub
            .get()
            .map_or_else(|| Flushable::flush(&*self.inner), |hub| hub.flush_now())
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
                    .or_insert_with(|| Arc::new(Mutex::default()))
                    .clone()
            },
            |arc| arc.clone(),
        )
    }

    pub(crate) fn record_commit(&self, key: &ResourceKey, final_len: u64) {
        let (root, path) = Self::resolve_refs(key);
        let arc = self.insert_or_get_entry(root, path);
        arc.lock().mark_committed(final_len);
        self.inner.dirty.store(true, Ordering::Release);
    }

    pub(crate) fn record_write(&self, key: &ResourceKey, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        let (root, path) = Self::resolve_refs(key);
        let arc = self.insert_or_get_entry(root, path);
        arc.lock().insert(range);
        self.inner.dirty.store(true, Ordering::Release);
    }

    pub(crate) fn remove(&self, key: &ResourceKey) {
        let (root, path) = Self::resolve_refs(key);
        if let Some(asset) = self.inner.assets.get(root) {
            asset.remove(path);
        }
    }

    fn resolve_refs(key: &ResourceKey) -> (&str, &str) {
        match key {
            ResourceKey::Relative {
                asset_root,
                rel_path,
            } => (asset_root, rel_path),
            ResourceKey::Absolute(path) => ("__absolute__", path.to_str().unwrap_or("")),
        }
    }
}

impl Flushable for InnerIndex {
    fn dirty(&self) -> &AtomicBool {
        &self.dirty
    }

    fn flush(&self) -> AssetsResult<()> {
        self.flush_with_durability(false)
    }

    fn flush_durable(&self) -> AssetsResult<()> {
        self.flush_with_durability(true)
    }

    fn name(&self) -> &'static str {
        "availability"
    }
}

#[cfg(target_arch = "wasm32")]
impl InnerIndex {
    fn flush_with_durability(&self, _durable: bool) -> AssetsResult<()> {
        self.dirty.store(false, Ordering::Release);
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
    index: AvailabilityIndex,
    key: ResourceKey,
}

impl ScopedAvailabilityObserver {
    pub(crate) fn new(key: ResourceKey, index: AvailabilityIndex) -> Arc<Self> {
        Arc::new(Self { index, key })
    }
}

impl AvailabilityObserver for ScopedAvailabilityObserver {
    fn on_commit(&self, final_len: u64) {
        self.index.record_commit(&self.key, final_len);
        if let Some(hub) = self.index.inner.hub.get() {
            hub.signal();
        }
    }

    fn on_write(&self, range: Range<u64>) {
        self.index.record_write(&self.key, range);
        if let Some(hub) = self.index.inner.hub.get() {
            hub.signal();
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use kithara_platform::{CancelToken, time::Duration};
    use kithara_storage::{Atomic, MmapOptions, MmapResource, OpenMode, Resource};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;

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
        let k1 = ResourceKey::relative("test_asset", "file1");
        let k2 = ResourceKey::relative("test_asset", "file2");

        idx.record_write(&k1, 0..10);
        idx.record_write(&k2, 20..30);

        assert!(idx.contains_range(&k1, 0..10));
        assert!(!idx.contains_range(&k1, 20..30));
        assert!(idx.contains_range(&k2, 20..30));
        assert!(!idx.contains_range(&k2, 0..10));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_new_is_empty() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");
        assert!(!idx.contains_range(&k, 0..10));
        assert_eq!(idx.final_len(&k), None);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_commit_sets_final_len_and_full_range() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");

        idx.record_commit(&k, 50);

        assert_eq!(idx.final_len(&k), Some(50));
        assert!(idx.contains_range(&k, 0..50));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_write_slow_then_fast_path() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");

        idx.record_write(&k, 0..10);
        assert!(idx.contains_range(&k, 0..10));

        idx.record_write(&k, 10..20);
        assert!(idx.contains_range(&k, 0..20));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_write_empty_range_is_noop() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");

        idx.record_write(&k, 10..10);
        assert!(!idx.contains_range(&k, 10..11));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_remove_clears_entry() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");

        idx.record_write(&k, 0..10);
        idx.remove(&k);

        assert!(!idx.contains_range(&k, 0..10));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_snapshot_and_seed_roundtrip() {
        let dir = TempDir::new().unwrap();
        let res: MmapResource = Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(dir.path().join("availability.bin"))
                .initial_len(4096)
                .mode(OpenMode::ReadWrite)
                .build(),
        )
        .unwrap();
        let atomic = Atomic::new(res);

        let idx1 = AvailabilityIndex::new();
        let k1 = ResourceKey::relative("test_asset", "file1");
        let k2 = ResourceKey::relative("test_asset", "file2");
        let k3 = ResourceKey::relative("test_asset", "file3");

        // k1: written then committed — a committed range round-trips.
        idx1.record_write(&k1, 0..10);
        idx1.record_commit(&k1, 10);
        // k2: committed via final length — round-trips.
        idx1.record_commit(&k2, 50);
        // k3: written but NEVER committed — the snapshot is a committed-only
        // crash-recovery contract, so an uncommitted partial write must NOT
        // round-trip (it would otherwise resurrect a partial segment whose
        // `.tmp` was never renamed).
        idx1.record_write(&k3, 0..10);

        idx1.persist_to(&atomic).unwrap();

        let idx2 = AvailabilityIndex::new();
        idx2.load_from(&atomic).unwrap();

        assert!(idx2.contains_range(&k1, 0..10));
        assert_eq!(idx2.final_len(&k2), Some(50));
        assert!(
            !idx2.contains_range(&k3, 0..10),
            "uncommitted partial write must not persist into the snapshot"
        );
        assert_eq!(idx2.final_len(&k3), None);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_empty_resource_loads_empty() {
        let dir = TempDir::new().unwrap();
        let res: MmapResource = Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(dir.path().join("availability.bin"))
                .mode(OpenMode::ReadWrite)
                .build(),
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
            CancelToken::never(),
            MmapOptions::for_path(dir.path().join("availability.bin"))
                .initial_len(4096)
                .mode(OpenMode::ReadWrite)
                .build(),
        )
        .unwrap();
        let atomic = Atomic::new(res);
        atomic.write_all(b"not valid bytes").unwrap();

        let idx = AvailabilityIndex::new();
        idx.load_from(&atomic).unwrap();
        assert!(idx.inner.assets.is_empty());
    }
}
