#![forbid(unsafe_code)]

//! Per-resource byte availability index.
//!
//! `Availability` is a single resource's snapshot of which byte ranges
//! have been written and whether it has been committed. Attempt #1's
//! retrospective split this into `availability.rs` + `availability_file.rs`
//! — attempt #2 mirrors the `lru.rs` / `pin.rs` pattern and keeps
//! everything (data type, aggregate, on-disk schema, load/store, scoped
//! observer) in a single file.
//!
//! ## Design
//!
//! - [`Availability`] holds per-resource state: the set of written byte
//!   ranges, the committed final length, and a committed flag.
//! - [`AvailabilityIndex`] is the aggregate — `Arc<InnerIndex>` where
//!   `InnerIndex::entries: DashMap<ResourceKey, Arc<Mutex<Availability>>>`.
//!   **No `Drop` impl**: all persistence is explicit via `checkpoint()`
//!   (Phase P-4), never from an `Arc` release path.
//! - [`ScopedAvailabilityObserver`] implements
//!   [`kithara_storage::AvailabilityObserver`] and routes `on_write` /
//!   `on_commit` hooks into `AvailabilityIndex::record_write` /
//!   `record_commit` for a captured `(ResourceKey, AvailabilityIndex)`
//!   pair. Storage stays agnostic to the asset-layer key type.
//! - [`AvailabilityFileV1`] + [`AvailabilityEntry`] are the on-disk
//!   schema. `load` / `store` go through `Atomic<R: ResourceExt>` the
//!   same way `LruIndex` / `PinsIndex` do — no `std::fs`, no
//!   `tempfile::NamedTempFile`, no parallel atomic-write implementations.

use std::{ops::Range, sync::Arc};

use dashmap::{DashMap, mapref::entry::Entry};
use kithara_bufpool::BytePool;
use kithara_platform::Mutex;
use kithara_storage::{Atomic, AvailabilityObserver, ResourceExt};
use rangemap::RangeSet;
use serde::{Deserialize, Serialize};

use crate::{error::AssetsResult, key::ResourceKey};

/// Schema version for the `_index/availability.bin` on-disk file.
const CURRENT_VERSION: u32 = 1;

// ──────────────────────────────────────────────────────────────────────
// Availability — per-resource byte snapshot
// ──────────────────────────────────────────────────────────────────────

/// Byte-level availability state for a single resource.
///
/// Mutated by [`AvailabilityIndex::record_write`] /
/// [`AvailabilityIndex::record_commit`] under a per-entry
/// [`kithara_platform::Mutex`]. Cloned on snapshot for persistence.
#[derive(Clone, Debug, Default)]
pub(crate) struct Availability {
    pub(crate) ranges: RangeSet<u64>,
    pub(crate) final_len: Option<u64>,
    pub(crate) committed: bool,
}

impl Availability {
    /// Record a newly-written byte range. Empty ranges are ignored.
    fn insert(&mut self, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        self.ranges.insert(range);
    }

    /// Mark the resource as committed with the given final length and
    /// record the full `0..final_len` range as available. Called from
    /// the observer on `Resource::commit(Some(final_len))` and from the
    /// Phase P-3 open-time seed path for pre-existing committed files.
    fn mark_committed(&mut self, final_len: u64) {
        self.committed = true;
        self.final_len = Some(final_len);
        if final_len > 0 {
            self.ranges.insert(0..final_len);
        }
    }

    /// Return `true` if every byte in `range` is present. Empty ranges
    /// are vacuously covered.
    fn contains(&self, range: &Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        self.ranges.gaps(range).next().is_none()
    }
}

// ──────────────────────────────────────────────────────────────────────
// AvailabilityIndex — aggregate across all resources in one store
// ──────────────────────────────────────────────────────────────────────

/// Opaque handle to the aggregate byte availability index.
///
/// Cloning this handle is cheap (`Arc` bump). All clones share the same
/// underlying `DashMap` of per-resource [`Availability`] states, so an
/// update through any clone is visible to all other clones — including
/// the one held by the `AssetStore` enum variant and the one captured
/// inside every live [`ScopedAvailabilityObserver`].
///
/// There is deliberately **no** `Drop` impl on the inner state. All
/// persistence goes through `AssetStore::checkpoint()` (Phase P-4) on a
/// thread the caller chooses, never from an `Arc` release path. This is
/// the structural fix for attempt #1's landmine L3 (audio-worker Drop
/// sync writes).
///
/// Public as a type so it can appear in `AssetStore` enum variants, but
/// all methods are `pub(crate)` — callers outside the crate should go
/// through [`crate::AssetStore::contains_range`] and friends.
#[derive(Clone)]
pub struct AvailabilityIndex {
    inner: Arc<InnerIndex>,
}

struct InnerIndex {
    entries: DashMap<ResourceKey, Arc<Mutex<Availability>>>,
}

impl AvailabilityIndex {
    /// Construct an empty, in-memory-only index. Persistence wiring is
    /// Phase P-4's responsibility.
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(InnerIndex {
                entries: DashMap::new(),
            }),
        }
    }

    /// Observer entry-point: record that `range` is now present for
    /// `key`. Called from [`ScopedAvailabilityObserver::on_write`] on
    /// every `Resource::write_at` of a store-owned resource — this is
    /// audio-adjacent hot path, keep the cost to one `DashMap` lookup
    /// and one `Mutex` lock for the common (entry exists) case.
    pub(crate) fn record_write(&self, key: &ResourceKey, range: Range<u64>) {
        if range.start >= range.end {
            return;
        }
        if let Some(arc) = self.clone_entry(key) {
            arc.lock_sync().insert(range);
            return;
        }
        let arc = self.insert_or_get_entry(key);
        arc.lock_sync().insert(range);
    }

    /// Observer entry-point: record that `key` has been committed with
    /// `final_len` bytes. Uses the same fast/slow split as
    /// [`Self::record_write`], but is called from
    /// `Resource::commit(Some(final_len))` which is much rarer than
    /// `write_at`.
    pub(crate) fn record_commit(&self, key: &ResourceKey, final_len: u64) {
        if let Some(arc) = self.clone_entry(key) {
            arc.lock_sync().mark_committed(final_len);
            return;
        }
        let arc = self.insert_or_get_entry(key);
        arc.lock_sync().mark_committed(final_len);
    }

    /// Return the set of byte ranges currently known to be present for
    /// `key`. Empty if the store has never seen the resource.
    pub(crate) fn available_ranges(&self, key: &ResourceKey) -> RangeSet<u64> {
        self.clone_entry(key)
            .map_or_else(RangeSet::default, |arc| arc.lock_sync().ranges.clone())
    }

    /// Return `true` when every byte in `range` is already present for
    /// the resource. Empty ranges are vacuously covered.
    pub(crate) fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        if range.start >= range.end {
            return true;
        }
        self.clone_entry(key)
            .is_some_and(|arc| arc.lock_sync().contains(&range))
    }

    /// Return the committed final length if the store has observed a
    /// `commit` for the resource.
    pub(crate) fn final_len(&self, key: &ResourceKey) -> Option<u64> {
        self.clone_entry(key)
            .and_then(|arc| arc.lock_sync().final_len)
    }

    /// Remove the resource from the aggregate.
    pub(crate) fn remove(&self, key: &ResourceKey) {
        self.inner.entries.remove(key);
    }

    /// Snapshot every entry into a `Vec<AvailabilityEntry>` for
    /// persistence. Holds the `DashMap` shard guards briefly, one at a
    /// time, and clones the inner state — callers on other keys are
    /// not blocked for the whole iteration.
    #[cfg_attr(not(test), expect(dead_code, reason = "wired in P-4 checkpoint API"))]
    pub(crate) fn snapshot(&self) -> Vec<AvailabilityEntry> {
        self.inner
            .entries
            .iter()
            .map(|r| {
                let av = r.value().lock_sync();
                AvailabilityEntry {
                    key: r.key().clone(),
                    ranges: av
                        .ranges
                        .iter()
                        .map(|range| (range.start, range.end))
                        .collect(),
                    final_len: av.final_len,
                    committed: av.committed,
                }
            })
            .collect()
    }

    /// Seed the aggregate from a persisted snapshot. Overwrites any
    /// existing in-memory entries for the same keys.
    #[cfg_attr(not(test), expect(dead_code, reason = "wired in P-4 checkpoint API"))]
    pub(crate) fn seed_from(&self, entries: Vec<AvailabilityEntry>) {
        for entry in entries {
            let mut av = Availability::default();
            for (start, end) in entry.ranges {
                av.insert(start..end);
            }
            av.final_len = entry.final_len;
            av.committed = entry.committed;
            self.inner
                .entries
                .insert(entry.key, Arc::new(Mutex::new(av)));
        }
    }

    /// Number of tracked resources. Used by `Debug` and by tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.entries.len()
    }

    // Fast path: look up an entry's `Arc` and release the DashMap
    // shard guard before the caller locks the inner mutex. Holding the
    // shard guard across a `Mutex::lock_sync()` would block any other
    // writer on the same shard for the duration of the lock — that was
    // the second contention bug attempt #1 stumbled into.
    fn clone_entry(&self, key: &ResourceKey) -> Option<Arc<Mutex<Availability>>> {
        let entry = self.inner.entries.get(key)?;
        let arc = entry.value().clone();
        drop(entry);
        Some(arc)
    }

    // Slow path: insert a fresh `Availability` entry when the key has
    // never been seen. Pays `ResourceKey::clone()` exactly once per
    // resource lifetime. Also releases the DashMap shard guard before
    // returning, same reasoning as `clone_entry`.
    fn insert_or_get_entry(&self, key: &ResourceKey) -> Arc<Mutex<Availability>> {
        match self.inner.entries.entry(key.clone()) {
            Entry::Occupied(occupied) => occupied.get().clone(),
            Entry::Vacant(vacant) => {
                let arc = Arc::new(Mutex::new(Availability::default()));
                vacant.insert(arc.clone());
                arc
            }
        }
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
            .field("tracked_resources", &self.inner.entries.len())
            .finish()
    }
}

// ──────────────────────────────────────────────────────────────────────
// ScopedAvailabilityObserver — storage hook → aggregate mutation
// ──────────────────────────────────────────────────────────────────────

/// `kithara_storage::AvailabilityObserver` implementation scoped to a
/// single `ResourceKey`. Constructed per-resource by the asset-layer
/// `DiskAssetStore::open_storage_resource` /
/// `MemAssetStore::acquire_resource_with_ctx` call sites: the
/// captured `(key, index)` pair routes every `on_write` / `on_commit`
/// event into `AvailabilityIndex::record_*`.
pub(crate) struct ScopedAvailabilityObserver {
    key: ResourceKey,
    index: AvailabilityIndex,
}

impl ScopedAvailabilityObserver {
    pub(crate) fn new(key: ResourceKey, index: AvailabilityIndex) -> Arc<Self> {
        Arc::new(Self { key, index })
    }
}

impl AvailabilityObserver for ScopedAvailabilityObserver {
    fn on_write(&self, range: Range<u64>) {
        self.index.record_write(&self.key, range);
    }

    fn on_commit(&self, final_len: u64) {
        self.index.record_commit(&self.key, final_len);
    }
}

// ──────────────────────────────────────────────────────────────────────
// AvailabilityFileV1 — on-disk schema + load/store via Atomic<R>
// ──────────────────────────────────────────────────────────────────────

/// Persisted representation of the aggregate index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AvailabilityFileV1 {
    pub(crate) version: u32,
    pub(crate) entries: Vec<AvailabilityEntry>,
}

/// One resource's availability snapshot as stored in `availability.bin`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AvailabilityEntry {
    pub(crate) key: ResourceKey,
    pub(crate) ranges: Vec<(u64, u64)>,
    pub(crate) final_len: Option<u64>,
    pub(crate) committed: bool,
}

/// Load the availability index from a persistent resource.
///
/// Missing / empty / corrupt / wrong-version files are treated as an
/// empty seed (best-effort), mirroring `LruIndex::load` and
/// `PinsIndex::load`.
///
/// # Errors
///
/// Returns `AssetsError` only if the underlying resource read itself
/// fails (I/O error, cancellation). A corrupt payload is not an error.
#[cfg_attr(not(test), expect(dead_code, reason = "wired in P-4 checkpoint API"))]
pub(crate) fn load<R: ResourceExt>(
    res: &Atomic<R>,
    pool: &BytePool,
) -> AssetsResult<Vec<AvailabilityEntry>> {
    let mut buf = pool.get();
    let n = res.read_into(&mut buf)?;
    if n == 0 {
        return Ok(Vec::new());
    }
    match postcard::from_bytes::<AvailabilityFileV1>(&buf[..n]) {
        Ok(file) if file.version == CURRENT_VERSION => Ok(file.entries),
        // Wrong version or corrupt bytes — treat as empty (best effort).
        _ => Ok(Vec::new()),
    }
}

/// Persist the given entries to the resource.
///
/// # Errors
///
/// Returns `AssetsError` if serialization or writing to the storage
/// resource fails.
#[cfg_attr(not(test), expect(dead_code, reason = "wired in P-4 checkpoint API"))]
pub(crate) fn store<R: ResourceExt>(
    res: &Atomic<R>,
    entries: &[AvailabilityEntry],
) -> AssetsResult<()> {
    let file = AvailabilityFileV1 {
        version: CURRENT_VERSION,
        entries: entries.to_vec(),
    };
    let bytes = postcard::to_allocvec(&file)?;
    res.write_all(&bytes)?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::time::Duration;

    use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    // ── Availability (per-resource) ───────────────────────────────────

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
        assert!(!a.contains(&(0..30)));
        assert!(!a.contains(&(5..25)));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_mark_committed_covers_full_range() {
        let mut a = Availability::default();
        a.mark_committed(64);
        assert!(a.committed);
        assert_eq!(a.final_len, Some(64));
        assert!(a.contains(&(0..64)));
        assert!(!a.contains(&(0..65)));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn availability_mark_committed_zero_len_has_no_range() {
        let mut a = Availability::default();
        a.mark_committed(0);
        assert!(a.committed);
        assert_eq!(a.final_len, Some(0));
        assert!(a.ranges.is_empty());
    }

    // ── AvailabilityIndex (aggregate) ─────────────────────────────────

    fn new_key(rel: &str) -> ResourceKey {
        ResourceKey::new(rel)
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_new_is_empty() {
        let idx = AvailabilityIndex::new();
        assert_eq!(idx.len(), 0);
        let key = new_key("foo.bin");
        assert!(idx.available_ranges(&key).is_empty());
        assert!(!idx.contains_range(&key, 0..1));
        assert_eq!(idx.final_len(&key), None);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_write_slow_then_fast_path() {
        let idx = AvailabilityIndex::new();
        let key = new_key("foo.bin");
        // First write: slow path (allocates entry).
        idx.record_write(&key, 0..10);
        assert_eq!(idx.len(), 1);
        assert!(idx.contains_range(&key, 0..10));
        // Second write: fast path (entry exists).
        idx.record_write(&key, 20..30);
        assert_eq!(idx.len(), 1);
        assert!(idx.contains_range(&key, 20..30));
        assert!(!idx.contains_range(&key, 0..30));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_write_empty_range_is_noop() {
        let idx = AvailabilityIndex::new();
        let key = new_key("foo.bin");
        idx.record_write(&key, 10..10);
        assert_eq!(idx.len(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_record_commit_sets_final_len_and_full_range() {
        let idx = AvailabilityIndex::new();
        let key = new_key("foo.bin");
        idx.record_commit(&key, 64);
        assert_eq!(idx.final_len(&key), Some(64));
        assert!(idx.contains_range(&key, 0..64));
        assert!(!idx.contains_range(&key, 0..65));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_per_key_isolation() {
        let idx = AvailabilityIndex::new();
        let a = new_key("a.bin");
        let b = new_key("b.bin");
        idx.record_write(&a, 0..10);
        assert!(idx.contains_range(&a, 0..10));
        assert!(!idx.contains_range(&b, 0..10));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_remove_clears_entry() {
        let idx = AvailabilityIndex::new();
        let key = new_key("foo.bin");
        idx.record_commit(&key, 32);
        assert_eq!(idx.final_len(&key), Some(32));
        idx.remove(&key);
        assert_eq!(idx.final_len(&key), None);
        assert!(idx.available_ranges(&key).is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn index_snapshot_and_seed_roundtrip() {
        let idx = AvailabilityIndex::new();
        let a = new_key("a.bin");
        let b = new_key("b.bin");
        idx.record_write(&a, 0..10);
        idx.record_write(&a, 20..30);
        idx.record_commit(&b, 64);

        let snap = idx.snapshot();
        assert_eq!(snap.len(), 2);

        let idx2 = AvailabilityIndex::new();
        idx2.seed_from(snap);
        assert!(idx2.contains_range(&a, 0..10));
        assert!(idx2.contains_range(&a, 20..30));
        assert!(!idx2.contains_range(&a, 0..30));
        assert_eq!(idx2.final_len(&b), Some(64));
        assert!(idx2.contains_range(&b, 0..64));
    }

    // ── On-disk schema load/store via Atomic<R> ───────────────────────

    fn create_test_resource(dir: &TempDir) -> MmapResource {
        let path = dir.path().join("availability.bin");
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_empty_resource_loads_empty() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir);
        let atomic = Atomic::new(res);
        let entries = load(&atomic, crate::byte_pool()).unwrap();
        assert!(entries.is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_roundtrip_via_atomic_resource() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir);
        let atomic = Atomic::new(res);
        let entries = vec![
            AvailabilityEntry {
                key: new_key("a.bin"),
                ranges: vec![(0, 100), (200, 300)],
                final_len: None,
                committed: false,
            },
            AvailabilityEntry {
                key: new_key("b.bin"),
                ranges: vec![(0, 64)],
                final_len: Some(64),
                committed: true,
            },
        ];
        store(&atomic, &entries).unwrap();
        let loaded = load(&atomic, crate::byte_pool()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].key, entries[0].key);
        assert_eq!(loaded[0].ranges, entries[0].ranges);
        assert!(!loaded[0].committed);
        assert_eq!(loaded[1].final_len, Some(64));
        assert!(loaded[1].committed);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_corrupt_payload_loads_empty() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir);
        res.write_all(b"not valid postcard bytes").unwrap();
        let atomic = Atomic::new(res);
        let entries = load(&atomic, crate::byte_pool()).unwrap();
        assert!(entries.is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn schema_wrong_version_loads_empty() {
        let dir = TempDir::new().unwrap();
        let res = create_test_resource(&dir);
        let wrong_version = AvailabilityFileV1 {
            version: 999,
            entries: vec![],
        };
        let bytes = postcard::to_allocvec(&wrong_version).unwrap();
        res.write_all(&bytes).unwrap();
        let atomic = Atomic::new(res);
        let entries = load(&atomic, crate::byte_pool()).unwrap();
        assert!(entries.is_empty());
    }
}
