#![forbid(unsafe_code)]

use std::{collections::HashMap, ops::Range, path::PathBuf};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use kithara_platform::sync::{Arc, Mutex, Retired};
use kithara_storage::AvailabilityObserver;
use rangemap::RangeSet;

use crate::{
    consts,
    index::persistence::{IndexCell, IndexState},
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
        let mut ranges = RangeSet::new();
        if final_len > 0 {
            ranges.insert(0..final_len);
        }
        if self.committed && self.final_len == Some(final_len) && self.ranges == ranges {
            return false;
        }
        self.committed = true;
        self.final_len = Some(final_len);
        self.ranges = ranges;
        true
    }
}

/// Opaque handle to the aggregate byte availability index.
#[derive(Clone)]
pub(crate) struct AvailabilityIndex {
    pub(super) inner: Arc<IndexCell<InnerIndex>>,
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
/// download and deletion paths, never on the audio thread. A reader racing a
/// writer must never end up the last owner of a displaced generation, so
/// writers hold what they displace in [`Retired`] lists until quiesced. An
/// [`Entry`] shared across tree generations dies with the last tree holding
/// it, on the writer: readers reach an entry only through a tree guard and
/// drop the snapshot guard first.
pub(super) type AssetTree = HashMap<String, Arc<HashMap<String, Entry>>>;

pub(super) struct InnerIndex {
    /// Maps `asset_root` -> `RelativePath` -> `Availability`
    pub(super) assets: ArcSwap<AssetTree>,
    /// Committed files still awaiting their durability barrier. The flush
    /// forces these down before writing the snapshot, so a resource is
    /// never named in the manifest ahead of its own bytes.
    pub(super) pending_durability: DashMap<PathBuf, ResourceKey>,
    /// Resource snapshots displaced by range updates, held until quiesced.
    retired_snapshots: Mutex<Retired<Availability>>,
    /// Trees displaced by structural edits, held until quiesced.
    retired_trees: Mutex<Retired<AssetTree>>,
}

impl AvailabilityIndex {
    pub(crate) fn new() -> Self {
        Self::default()
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
        let mut removed = false;
        self.edit_tree(|tree| removed = tree.remove(asset_root).is_some());
        if removed {
            self.inner.mark_dirty();
        }
    }

    /// Called from the decode produce path (`phase_at` cascade): reads the
    /// snapshots through guards only, since writers retire what they displace.
    pub(crate) fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        let (root, path) = Self::resolve_refs(key);
        let tree = self.inner.state.assets.load();
        tree.get(root)
            .and_then(|asset| asset.get(path))
            .is_some_and(|entry| entry.load().contains(&range))
    }

    /// Publish a structural change to the snapshot tree and retire the tree
    /// it displaced.
    ///
    /// Rebuilds under [`ArcSwap::rcu`]: readers keep loading complete trees
    /// throughout, and a racing edit re-runs against the tree that won. The
    /// closure must therefore be idempotent - every caller here is (map
    /// inserts and removals).
    pub(super) fn edit_tree(&self, mut edit: impl FnMut(&mut AssetTree)) {
        let displaced = self.inner.state.assets.rcu(|tree| {
            let mut next = AssetTree::clone(tree);
            edit(&mut next);
            next
        });
        self.inner.state.retired_trees.lock().retire(displaced);
    }

    fn entry(&self, asset_root: &str, path: &str) -> Option<Entry> {
        self.inner
            .state
            .assets
            .load()
            .get(asset_root)
            .and_then(|asset| asset.get(path))
            .cloned()
    }

    /// Lock-free like [`Self::contains_range`]: guards only, never ownership.
    pub(crate) fn final_len(&self, key: &ResourceKey) -> Option<u64> {
        let (root, path) = Self::resolve_refs(key);
        let tree = self.inner.state.assets.load();
        tree.get(root)
            .and_then(|asset| asset.get(path))
            .and_then(|entry| entry.load().final_len)
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

    pub(crate) fn record_commit(&self, key: &ResourceKey, final_len: u64) {
        if self.update_commit(key, final_len) {
            self.inner.mark_dirty();
        }
    }

    fn update_commit(&self, key: &ResourceKey, final_len: u64) -> bool {
        let (root, path) = Self::resolve_refs(key);
        let entry = self.insert_or_get_entry(root, path);
        self.update(&entry, |next| next.mark_committed(final_len))
    }

    /// Enqueue a published file before recording its committed availability.
    /// The entry guard covers enqueue and availability publication. A flush
    /// removes its cohort before any barrier, so later obligations for the
    /// same path remain queued.
    fn record_file_commit(&self, key: &ResourceKey, path: PathBuf, final_len: u64) {
        let pending = self
            .inner
            .state
            .pending_durability
            .entry(path)
            .or_insert_with(|| key.clone());
        self.update_commit(key, final_len);
        drop(pending);
        self.inner.mark_dirty();
    }

    pub(crate) fn record_write(&self, key: &ResourceKey, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        let (root, path) = Self::resolve_refs(key);
        let entry = self.insert_or_get_entry(root, path);
        if self.update(&entry, |next| next.insert(range.clone())) {
            self.inner.mark_dirty();
        }
    }

    pub(crate) fn remove(&self, key: &ResourceKey) {
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
            self.inner.mark_dirty();
        }
    }

    pub(super) fn resolve_refs(key: &ResourceKey) -> (&str, &str) {
        match key.kind() {
            ResourceKeyKind::Relative {
                asset_root,
                rel_path,
            } => (asset_root, rel_path),
            ResourceKeyKind::Absolute(path) => (consts::ABSOLUTE_ROOT, path.to_str().unwrap_or("")),
        }
    }

    /// Keep only the entries `keep` accepts, by asset root and relative path.
    pub(crate) fn retain<F: Fn(&str, &str) -> bool>(&self, keep: F) {
        let mut dropped = false;
        self.edit_tree(|tree| {
            tree.retain(|root, entries| {
                let kept: HashMap<String, Entry> = entries
                    .iter()
                    .filter(|(path, _)| keep(root, path))
                    .map(|(path, entry)| (path.clone(), Arc::clone(entry)))
                    .collect();
                dropped |= kept.len() != entries.len();
                *entries = Arc::new(kept);
                !entries.is_empty()
            });
        });
        if dropped {
            self.inner.mark_dirty();
        }
    }

    /// Apply a mutation to one resource's availability, publish the result,
    /// and retire the snapshot it displaced.
    ///
    /// Clone-update-swap under [`ArcSwap::rcu`]: a racing writer makes the
    /// loser re-run against the winner's state, so no update is lost.
    /// Returns what the mutation returned on its winning run.
    fn update(&self, entry: &Entry, mut mutate: impl FnMut(&mut Availability) -> bool) -> bool {
        let mut changed = false;
        let displaced = entry.rcu(|current| {
            let mut next = Availability::clone(current);
            changed = mutate(&mut next);
            next
        });
        self.inner.state.retired_snapshots.lock().retire(displaced);
        changed
    }
}

impl IndexState for InnerIndex {
    const NAME: &'static str = "availability";
}

impl Default for AvailabilityIndex {
    fn default() -> Self {
        Self {
            inner: Arc::new(IndexCell::new(InnerIndex {
                assets: ArcSwap::from_pointee(AssetTree::new()),
                pending_durability: DashMap::new(),
                retired_snapshots: Mutex::new(Retired::default()),
                retired_trees: Mutex::new(Retired::default()),
            })),
        }
    }
}

impl std::fmt::Debug for AvailabilityIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvailabilityIndex")
            .field("tracked_assets", &self.inner.state.assets.load().len())
            .finish()
    }
}

/// `kithara_storage::AvailabilityObserver` implementation scoped to a
/// single `ResourceKey`.
pub(crate) struct ScopedAvailabilityObserver {
    index: AvailabilityIndex,
    /// Backing file, when the resource has one whose durability the index
    /// must force before naming it in the manifest.
    path: Option<PathBuf>,
    key: ResourceKey,
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
        match self.path.as_ref() {
            Some(path) => self
                .index
                .record_file_commit(&self.key, path.clone(), final_len),
            None => self.index.record_commit(&self.key, final_len),
        }
    }

    fn on_write(&self, range: Range<u64>) {
        self.index.record_write(&self.key, range);
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::Ordering;

    use kithara_platform::{sync::Weak, time::Duration};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;

    use super::*;
    use crate::index::persistence::{Flushable, IndexFile};

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
    #[case::empty(0)]
    #[case::shorter(3)]
    fn committed_ranges_match_final_length_after_shrink(#[case] final_len: u64) {
        let mut availability = Availability::default();
        availability.mark_committed(7);

        assert!(availability.mark_committed(final_len));

        assert_eq!(availability.final_len, Some(final_len));
        assert!(availability.contains(&(0..final_len)));
        assert!(!availability.contains(&(final_len..7)));
        assert_eq!(availability.ranges.is_empty(), final_len == 0);
        assert!(!availability.mark_committed(final_len));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    #[case::empty(0)]
    #[case::shorter(3)]
    fn committing_the_same_length_removes_legacy_range_tails(#[case] final_len: u64) {
        let mut availability = Availability::default();
        availability.mark_committed(final_len);
        availability.insert(final_len..7);

        assert!(availability.mark_committed(final_len));

        assert!(!availability.contains(&(final_len..7)));
        assert_eq!(availability.ranges.is_empty(), final_len == 0);
        assert!(!availability.mark_committed(final_len));
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

    /// A read never takes ownership of a generation, however long the write
    /// side stays quiet.
    ///
    /// The produce path reads at audio-tick cadence (~94 ticks/s at 48 kHz
    /// with 512-frame blocks) while writes arrive at download cadence, so a
    /// stretch served from cache issues thousands of reads with no write in
    /// between. The burst is ten seconds of ticks over both snapshot reads.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn a_read_burst_never_leaks_a_generation() {
        const TICKS_PER_SECOND: usize = 94;
        const BURST: usize = TICKS_PER_SECOND * 10;

        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");
        idx.record_commit(&k, 10);
        let tree = idx.inner.state.assets.load_full();
        let snapshot = idx
            .entry("test_asset", "file1")
            .expect("a committed resource has an entry")
            .load_full();
        let tree_owners = Arc::strong_count(&tree);
        let snapshot_owners = Arc::strong_count(&snapshot);

        let resolved = (0..BURST)
            .filter(|_| idx.contains_range(&k, 0..10) && idx.final_len(&k) == Some(10))
            .count();

        assert_eq!(
            resolved, BURST,
            "every read in the burst resolves the resource snapshot"
        );
        assert_eq!(
            Arc::strong_count(&tree),
            tree_owners,
            "{BURST} reads with no intervening write took ownership of the tree"
        );
        assert_eq!(
            Arc::strong_count(&snapshot),
            snapshot_owners,
            "{BURST} reads with no intervening write took ownership of the resource snapshot"
        );
    }

    /// A reader overlapping range writes is never the last owner of a snapshot
    /// they displaced: its guard drop leaves the snapshot to the writer, and
    /// the first write after that drop frees it.
    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn displaced_snapshot_is_freed_by_the_writer() {
        let idx = AvailabilityIndex::new();
        let k = ResourceKey::relative("test_asset", "file1");
        idx.record_write(&k, 0..10);
        let tree = idx.inner.state.assets.load();
        let reader = tree
            .get("test_asset")
            .and_then(|asset| asset.get("file1"))
            .expect("a written resource has an entry")
            .load();
        let displaced: Weak<Availability> = Arc::downgrade(&reader);

        idx.record_write(&k, 10..20);
        idx.record_write(&k, 20..30);
        drop(reader);
        drop(tree);
        assert!(
            displaced.upgrade().is_some(),
            "a reader guard drop never frees the displaced snapshot"
        );

        idx.record_write(&k, 30..40);
        assert!(
            displaced.upgrade().is_none(),
            "the next write frees the quiesced snapshot"
        );
    }

    /// A reader overlapping structural edits is never the last owner of a tree
    /// they displaced: its guard drop leaves the tree to the writer, and the
    /// first edit after that drop frees it.
    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn displaced_tree_is_freed_by_the_writer() {
        let idx = AvailabilityIndex::new();
        idx.record_write(&ResourceKey::relative("test_asset", "file1"), 0..10);
        let reader = idx.inner.state.assets.load();
        let displaced: Weak<AssetTree> = Arc::downgrade(&reader);

        idx.record_write(&ResourceKey::relative("test_asset", "file2"), 0..10);
        idx.record_write(&ResourceKey::relative("test_asset", "file3"), 0..10);
        drop(reader);
        assert!(
            displaced.upgrade().is_some(),
            "a reader guard drop never frees the displaced tree"
        );

        idx.record_write(&ResourceKey::relative("test_asset", "file4"), 0..10);
        assert!(
            displaced.upgrade().is_none(),
            "the next edit frees the quiesced tree"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_snapshot_and_seed_roundtrip() {
        let dir = TempDir::new().unwrap();
        let file = IndexFile::new(dir.path().join("availability.bin"));
        let pools = crate::test_pools::pools();
        let mut buf = crate::test_pools::byte_buffer(&pools);

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

        idx1.persist_to(&file).unwrap();

        let idx2 = AvailabilityIndex::new();
        idx2.load_from(&file, &mut buf).unwrap();

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
        let file = IndexFile::new(dir.path().join("availability.bin"));
        let pools = crate::test_pools::pools();
        let mut buf = crate::test_pools::byte_buffer(&pools);

        let idx = AvailabilityIndex::new();
        idx.load_from(&file, &mut buf).unwrap();
        assert!(idx.inner.state.assets.load().is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_corrupt_payload_loads_empty() {
        let dir = TempDir::new().unwrap();
        let file = IndexFile::new(dir.path().join("availability.bin"));
        let pools = crate::test_pools::pools();
        let mut buf = crate::test_pools::byte_buffer(&pools);
        std::fs::write(dir.path().join("availability.bin"), b"not valid bytes").unwrap();

        let idx = AvailabilityIndex::new();
        idx.load_from(&file, &mut buf).unwrap();
        assert!(idx.inner.state.assets.load().is_empty());
    }
}
