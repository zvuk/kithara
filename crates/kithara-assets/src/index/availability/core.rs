#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Weak;
use std::{
    collections::HashMap,
    ops::Range,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use arc_swap::{ArcSwap, Guard};
use dashmap::DashSet;
use kithara_platform::sync::Arc;
use kithara_storage::AvailabilityObserver;
use rangemap::RangeSet;

use super::retire::{RETIRE_CAPACITY, Retired};
use crate::{
    error::AssetsResult,
    index::persistence::{FlushHub, Flushable},
    layout::{ResourceKey, ResourceKeyKind},
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

    pub(super) fn insert(&mut self, range: Range<u64>) -> bool {
        if range.start >= range.end || self.contains(&range) {
            return false;
        }
        self.ranges.insert(range);
        true
    }

    pub(super) fn mark_committed(&mut self, final_len: u64) -> bool {
        let range = 0..final_len;
        if self.committed && self.final_len == Some(final_len) && self.contains(&range) {
            return false;
        }
        self.committed = true;
        self.final_len = Some(final_len);
        if final_len > 0 {
            self.ranges.insert(range);
        }
        true
    }
}

/// Opaque handle to the aggregate byte availability index.
#[derive(Clone)]
pub(crate) struct AvailabilityIndex {
    pub(super) inner: Arc<InnerIndex>,
}

/// One resource's availability, readable without blocking.
///
/// The decode produce path asks `contains_range` from the audio thread (the
/// `phase_at` cascade), where taking a lock is a real-time violation: under
/// write contention `parking_lot` parks or yields, and the audio thread stalls
/// on the downloader. Readers therefore get a snapshot; writers publish a new
/// one. A reader racing a writer sees the state from a moment ago, which for
/// "is this byte on disk yet" is indistinguishable from having asked a moment
/// ago.
pub(super) type Entry = Arc<ArcSwap<Availability>>;

/// Immutable snapshot tree: `asset_root` -> `RelativePath` -> [`Entry`].
///
/// Structural changes (a new resource, a deletion) publish a rebuilt tree;
/// range updates swap only the resource's own [`Entry`]. Both happen on
/// download and deletion paths, never on the audio thread. Publication is
/// only half of the contract: a reader racing a writer can end up the last
/// owner of the replaced generation, and its guard drop would then free the
/// tree on the audio thread. Produce-core reads therefore park their
/// snapshots in [`Retired`] and the write side pays the frees when it drains.
pub(super) type AssetTree = HashMap<String, Arc<HashMap<String, Entry>>>;

/// The asset root an absolute key is filed under.
pub(crate) const ABSOLUTE_ROOT: &str = "__absolute__";

pub(super) struct InnerIndex {
    /// Maps `asset_root` -> `RelativePath` -> `Availability`
    pub(super) assets: ArcSwap<AssetTree>,
    /// Snapshots parked by produce-core reads, freed by write-side drains.
    pub(super) retired: Retired,
    /// `true` when the in-memory aggregate has uncommitted writes
    /// since the last successful flush.
    pub(super) dirty: AtomicBool,
    /// Set by [`AvailabilityIndex::attach_to`]. While `None`,
    /// `ScopedAvailabilityObserver` falls back to the legacy
    /// "explicit checkpoint only" contract — every observer event
    /// just marks `dirty` so the next call to
    /// [`AvailabilityIndex::flush`] writes the snapshot.
    pub(super) hub: OnceLock<Arc<FlushHub>>,
    /// Committed files still awaiting their durability barrier. The flush
    /// forces these down before writing the snapshot, so a resource is
    /// never named in the manifest ahead of its own bytes.
    pub(super) pending_durability: DashSet<PathBuf>,
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
                assets: ArcSwap::from_pointee(AssetTree::new()),
                retired: Retired::new(RETIRE_CAPACITY),
                #[cfg(not(target_arch = "wasm32"))]
                persist: OnceLock::new(),
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
                pending_durability: DashSet::new(),
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
        self.entry(root, path)
            .map_or_else(RangeSet::new, |entry| entry.load().ranges.clone())
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
        self.inner.retired.drain();
        let mut removed = false;
        self.edit_tree(|tree| removed = tree.remove(asset_root).is_some());
        if removed {
            self.mark_dirty();
        }
    }

    /// Called from the decode produce path (`phase_at` cascade): reads the
    /// snapshots in place and parks them instead of dropping, so a read
    /// racing a writer never frees a replaced generation on the audio thread.
    pub(crate) fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        let (root, path) = Self::resolve_refs(key);
        let tree = self.inner.assets.load();
        let contains = tree
            .get(root)
            .and_then(|asset| asset.get(path))
            .is_some_and(|entry| {
                let snapshot = entry.load();
                let contains = snapshot.contains(&range);
                self.inner
                    .retired
                    .retire_availability(Guard::into_inner(snapshot));
                contains
            });
        self.inner.retired.retire_tree(Guard::into_inner(tree));
        contains
    }

    /// Also on the produce path — see [`Self::contains_range`].
    pub(crate) fn final_len(&self, key: &ResourceKey) -> Option<u64> {
        let (root, path) = Self::resolve_refs(key);
        let tree = self.inner.assets.load();
        let len = tree
            .get(root)
            .and_then(|asset| asset.get(path))
            .and_then(|entry| {
                let snapshot = entry.load();
                let len = snapshot.final_len;
                self.inner
                    .retired
                    .retire_availability(Guard::into_inner(snapshot));
                len
            });
        self.inner.retired.retire_tree(Guard::into_inner(tree));
        len
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
        if let Some(hub) = self.inner.hub.get() {
            return hub.flush_now();
        }
        if !self.inner.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let result = Flushable::flush(&*self.inner);
        if result.is_err() {
            self.inner.dirty.store(true, Ordering::Release);
        }
        result
    }

    fn entry(&self, asset_root: &str, path: &str) -> Option<Entry> {
        self.inner
            .assets
            .load()
            .get(asset_root)
            .and_then(|asset| asset.get(path))
            .cloned()
    }

    fn insert_or_get_entry(&self, asset_root: &str, path: &str) -> Entry {
        if let Some(entry) = self.entry(asset_root, path) {
            return entry;
        }
        let fresh = Entry::default();
        let mut winner = Arc::clone(&fresh);
        self.edit_tree(|tree| {
            let asset = tree.entry(asset_root.to_owned()).or_default();
            winner = asset.get(path).cloned().unwrap_or_else(|| {
                let mut next = HashMap::clone(asset);
                next.insert(path.to_owned(), Arc::clone(&fresh));
                *asset = Arc::new(next);
                Arc::clone(&fresh)
            });
        });
        winner
    }

    /// Publish a structural change to the snapshot tree.
    ///
    /// Rebuilds under [`ArcSwap::rcu`]: readers keep loading complete trees
    /// throughout, and a racing edit re-runs against the tree that won. The
    /// closure must therefore be idempotent — every caller here is (map
    /// insert-if-absent and removals).
    pub(super) fn edit_tree(&self, mut edit: impl FnMut(&mut AssetTree)) {
        self.inner.assets.rcu(|tree| {
            let mut next = AssetTree::clone(tree);
            edit(&mut next);
            next
        });
    }

    pub(super) fn mark_dirty(&self) {
        self.inner.dirty.store(true, Ordering::Release);
        if let Some(hub) = self.inner.hub.get() {
            hub.signal();
        }
    }

    /// Enqueue a committed file for the durability barrier. The manifest
    /// flush pays one barrier per queued file and only then names them, so
    /// the resource-write path never waits on the medium itself.
    pub(crate) fn record_pending_durability(&self, path: PathBuf) {
        self.inner.pending_durability.insert(path);
    }

    pub(crate) fn record_commit(&self, key: &ResourceKey, final_len: u64) {
        self.inner.retired.drain();
        let (root, path) = Self::resolve_refs(key);
        let entry = self.insert_or_get_entry(root, path);
        if update(&entry, |next| next.mark_committed(final_len)) {
            self.mark_dirty();
        }
    }

    pub(crate) fn record_write(&self, key: &ResourceKey, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        // The write side pays the frees the produce-core reads parked.
        self.inner.retired.drain();
        let (root, path) = Self::resolve_refs(key);
        let entry = self.insert_or_get_entry(root, path);
        if update(&entry, |next| next.insert(range.clone())) {
            self.mark_dirty();
        }
    }

    pub(crate) fn remove(&self, key: &ResourceKey) {
        self.inner.retired.drain();
        let (root, path) = Self::resolve_refs(key);
        let mut removed = false;
        self.edit_tree(|tree| {
            removed = false;
            if let Some(asset) = tree.get_mut(root)
                && asset.contains_key(path)
            {
                let mut next = HashMap::clone(asset);
                next.remove(path);
                *asset = Arc::new(next);
                removed = true;
            }
        });
        if removed {
            self.mark_dirty();
        }
    }

    fn resolve_refs(key: &ResourceKey) -> (&str, &str) {
        match key.kind() {
            ResourceKeyKind::Relative {
                asset_root,
                rel_path,
            } => (asset_root, rel_path),
            ResourceKeyKind::Absolute(path) => (ABSOLUTE_ROOT, path.to_str().unwrap_or("")),
        }
    }
}

/// Apply a mutation to one resource's availability and publish the result.
///
/// Clone-update-swap under [`ArcSwap::rcu`]: a racing writer makes the loser
/// re-run against the winner's state, so no update is lost — the same
/// guarantee the mutex gave, now without a lock for readers to block on.
/// Returns what the mutation returned on its winning run.
fn update(entry: &Entry, mut mutate: impl FnMut(&mut Availability) -> bool) -> bool {
    let mut changed = false;
    entry.rcu(|current| {
        let mut next = Availability::clone(current);
        changed = mutate(&mut next);
        next
    });
    changed
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
            .field("tracked_assets", &self.inner.assets.load().len())
            .finish()
    }
}

/// `kithara_storage::AvailabilityObserver` implementation scoped to a
/// single `ResourceKey`.
pub(crate) struct ScopedAvailabilityObserver {
    index: AvailabilityIndex,
    key: ResourceKey,
    /// Backing file, when the resource has one whose durability the index
    /// must force before naming it in the manifest.
    path: Option<PathBuf>,
}

impl ScopedAvailabilityObserver {
    pub(crate) fn new(key: ResourceKey, index: AvailabilityIndex) -> Arc<Self> {
        Arc::new(Self {
            index,
            key,
            path: None,
        })
    }

    /// Observer for a file-backed resource: its commit enqueues the file for
    /// the durability barrier the manifest flush pays on everyone's behalf.
    pub(crate) fn for_file(key: ResourceKey, index: AvailabilityIndex, path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            index,
            key,
            path: Some(path),
        })
    }
}

impl AvailabilityObserver for ScopedAvailabilityObserver {
    fn on_commit(&self, final_len: u64) {
        if let Some(path) = self.path.as_ref() {
            self.index.record_pending_durability(path.clone());
        }
        self.index.record_commit(&self.key, final_len);
    }

    fn on_write(&self, range: Range<u64>) {
        self.index.record_write(&self.key, range);
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
    fn unchanged_records_do_not_dirty_the_index() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");

        idx.record_write(&k, 0..10);
        idx.record_commit(&k, 10);
        idx.flush().unwrap();
        assert!(!idx.inner.dirty.load(Ordering::Acquire));

        idx.record_write(&k, 0..10);
        idx.record_commit(&k, 10);

        assert!(!idx.inner.dirty.load(Ordering::Acquire));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn flush_source_does_not_clear_a_new_dirty_signal() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");

        idx.record_write(&k, 0..10);
        assert!(idx.inner.dirty.swap(false, Ordering::AcqRel));
        idx.remove(&k);
        Flushable::flush(&*idx.inner).unwrap();

        assert!(idx.inner.dirty.load(Ordering::Acquire));
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
    fn a_read_parks_its_snapshots_for_the_write_side() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");
        idx.record_write(&k, 0..10);
        assert!(idx.inner.retired.is_empty());

        let _ = idx.contains_range(&k, 0..10);

        assert!(!idx.inner.retired.is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn a_write_drains_the_parked_snapshots() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");
        idx.record_write(&k, 0..10);
        let _ = idx.contains_range(&k, 0..10);

        idx.record_write(&k, 10..20);

        assert!(idx.inner.retired.is_empty());
    }

    /// A read never leaks a generation, however long the write side stays quiet.
    ///
    /// Every read parks two references - the tree and the resource snapshot -
    /// so the free lands off the audio thread, and only the write side drains
    /// them. The produce path reads at audio-tick cadence (~94 ticks/s at
    /// 48 kHz with 512-frame blocks) while writes arrive at download cadence,
    /// so a stretch served from cache issues thousands of reads with no drain
    /// between them. The bin is bounded and overflow does not free, it
    /// *forgets*: a forgotten generation is unreachable memory that no later
    /// drain can recover.
    ///
    /// Measured in the field on 2026-08-20: 844 overflow warnings in two
    /// minutes of HLS playback. The burst below is ten seconds of produce
    /// ticks, deliberately not derived from `RETIRE_CAPACITY` - raising the
    /// capacity moves the threshold, it does not bound the read:write ratio.
    ///
    /// `#[ignore]`d, not deleted: falsified locally at 940 reads. Removing the
    /// leak means the reader stops taking ownership per read, which is a
    /// redesign of the produce-path read contract, not a patch.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    #[ignore = "pins real regression — a read parks two references while only \
                writes drain the bounded bin, so ordinary playback overflows it \
                and mem::forget leaks a generation for good; unignore when \
                quiescent-state reclamation replaces the retire bin"]
    fn a_read_burst_never_leaks_a_generation() {
        const TICKS_PER_SECOND: usize = 94;
        const BURST: usize = TICKS_PER_SECOND * 10;

        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");
        idx.record_write(&k, 0..10);

        for _ in 0..BURST {
            assert!(idx.contains_range(&k, 0..10));
        }

        assert!(
            !idx.inner.retired.overflowed(),
            "{BURST} reads with no intervening write leaked a generation"
        );
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
        assert!(idx.inner.assets.load().is_empty());
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
        assert!(idx.inner.assets.load().is_empty());
    }
}
