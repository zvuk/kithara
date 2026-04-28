#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::BytePool;
use kithara_platform::Mutex;
use kithara_storage::{Atomic, MmapResource, ResourceExt, StorageError};
use tokio_util::sync::CancellationToken;

use super::{
    flush::{FlushHub, Flushable, signal_or_flush_sync},
    persist,
    schema::{LruEntryFile, LruIndexFile},
};
use crate::error::{AssetsError, AssetsResult};

/// Eviction configuration for an assets store decorator.
#[derive(Clone, Debug, Default)]
pub struct EvictConfig {
    pub max_assets: Option<usize>,
    pub max_bytes: Option<u64>,
}

/// In-memory + best-effort disk-backed LRU index over `asset_root`.
///
/// Architecturally symmetric to [`AvailabilityIndex`](super::AvailabilityIndex)
/// and [`PinsIndex`](super::PinsIndex): the `Arc` is encapsulated
/// **inside** the type, [`Clone`] is cheap (atomic refcount bump),
/// every mutation flushes the optional disk-backed [`Atomic`]
/// tempfile.
///
/// Persistence is **lazy**: the disk file is materialised only on the
/// first [`Self::flush`]. A pre-existing on-disk file from a previous
/// run is opened eagerly during [`Self::with_persist_at`] for
/// hydration, but a fresh build does not touch the filesystem until a
/// real touch/remove happens.
///
/// Two call-sites share a single instance per `cache_dir`:
///   * `EvictAssets` (touch/remove during open and eviction),
///   * `DiskAssetDeleter` (drop entry when an `asset_root` is fully removed).
#[derive(Clone)]
pub(crate) struct LruIndex {
    inner: Arc<LruInner>,
}

impl std::fmt::Debug for LruIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LruIndex")
            .field("len", &self.inner.state.try_lock().map(|s| s.len()).ok())
            .field("persist", &self.inner.persist.is_some())
            .finish()
    }
}

struct LruInner {
    state: Mutex<LruState>,
    persist: Option<LruPersist>,
    /// Set by [`LruIndex::attach_to`]. While `None`, mutators flush
    /// inline through [`Flushable::flush`] — matches the historical
    /// inline-flush behaviour for ad-hoc tests.
    hub: OnceLock<Arc<FlushHub>>,
    dirty: AtomicBool,
}

struct LruPersist {
    path: PathBuf,
    cancel: CancellationToken,
    res: OnceLock<Atomic<MmapResource>>,
}

impl LruIndex {
    /// Construct an ephemeral index (mem backend mode).
    pub(crate) fn ephemeral() -> Self {
        Self {
            inner: Arc::new(LruInner {
                state: Mutex::new(LruState::default()),
                persist: None,
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
            }),
        }
    }

    /// Construct a disk-backed index rooted at `path`.
    ///
    /// If the file already exists and is non-empty it is opened and
    /// hydrated synchronously. Otherwise the disk file is **not**
    /// materialised — it appears on the first [`Self::touch`] /
    /// [`Self::remove`].
    pub(crate) fn with_persist_at(
        path: PathBuf,
        cancel: CancellationToken,
        pool: &BytePool,
    ) -> Self {
        let (initial, opened) = hydrate_existing(&path, &cancel, pool);
        Self {
            inner: Arc::new(LruInner {
                state: Mutex::new(initial),
                persist: Some(LruPersist {
                    path,
                    cancel,
                    res: opened.map_or_else(OnceLock::new, |a| {
                        let cell = OnceLock::new();
                        cell.set(a)
                            .unwrap_or_else(|_| unreachable!("freshly created cell"));
                        cell
                    }),
                }),
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
            }),
        }
    }

    /// Bind this index to a [`FlushHub`] for coordinated flushing.
    /// Called once per index instance; subsequent calls are no-ops.
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        if self.inner.hub.set(Arc::clone(hub)).is_err() {
            return;
        }
        hub.register(Arc::downgrade(&self.inner) as Weak<dyn Flushable>);
    }

    /// Touch (mark as most-recent) an asset. Returns `true` if a new
    /// entry was created.
    ///
    /// Disk-backed instances flush the snapshot synchronously before
    /// returning; an `Err` here means the touch is **not** durable.
    /// Ephemeral instances cannot fail.
    ///
    /// # Errors
    ///
    /// Propagates [`AssetsError`] when the on-disk flush fails.
    pub(crate) fn touch(&self, asset_root: &str, bytes_hint: Option<u64>) -> AssetsResult<bool> {
        let created = {
            let mut st = self.inner.state.lock_sync();
            st.touch(asset_root, bytes_hint)
        };
        signal_or_flush_sync(self.inner.hub.get(), &*self.inner)?;
        Ok(created)
    }

    /// Remove an asset from the index (eviction or full deletion).
    ///
    /// Same durability contract as [`Self::touch`].
    ///
    /// # Errors
    ///
    /// Propagates [`AssetsError`] when the on-disk flush fails.
    pub(crate) fn remove(&self, asset_root: &str) -> AssetsResult<()> {
        let removed = {
            let mut st = self.inner.state.lock_sync();
            st.remove(asset_root)
        };
        if removed {
            signal_or_flush_sync(self.inner.hub.get(), &*self.inner)?;
        }
        Ok(())
    }

    /// Update the cached size of an existing entry without bumping the
    /// LRU clock. Use when the byte total changes but recency must not
    /// (e.g. segment commits add bytes to an already-tracked asset).
    ///
    /// Returns `true` if the entry existed and the byte total actually
    /// changed; only that path triggers a flush. If no entry exists or
    /// the value is identical, this is a pure read.
    ///
    /// # Errors
    ///
    /// Propagates [`AssetsError`] when the on-disk flush fails.
    pub(crate) fn update_bytes(&self, asset_root: &str, bytes: u64) -> AssetsResult<bool> {
        let changed = {
            let mut st = self.inner.state.lock_sync();
            st.update_bytes(asset_root, bytes)
        };
        if changed {
            signal_or_flush_sync(self.inner.hub.get(), &*self.inner)?;
        }
        Ok(changed)
    }

    /// Compute eviction candidates in LRU order (oldest first) under
    /// the given config. Pure in-memory read — never fails.
    pub(crate) fn eviction_candidates(
        &self,
        cfg: &EvictConfig,
        pinned: &HashSet<String>,
    ) -> Vec<String> {
        let st = self.inner.state.lock_sync();
        st.eviction_candidates(cfg, pinned)
    }

    /// Return total bytes across all assets (best-effort).
    pub(crate) fn total_bytes_best_effort(&self) -> u64 {
        self.inner
            .state
            .lock_sync()
            .by_root
            .values()
            .filter_map(|e| e.bytes)
            .sum()
    }
}

impl Flushable for LruInner {
    fn name(&self) -> &'static str {
        "lru"
    }

    fn dirty(&self) -> &AtomicBool {
        &self.dirty
    }

    fn flush(&self) -> AssetsResult<()> {
        let Some(persist) = self.persist.as_ref() else {
            self.dirty.store(false, Ordering::Release);
            return Ok(());
        };
        let snapshot = self.state.lock_sync().clone();
        let atomic = persist::init_atomic(&persist.res, &persist.path, &persist.cancel)?;
        write_state(atomic, &snapshot)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }
}

fn hydrate_existing(
    path: &std::path::Path,
    cancel: &CancellationToken,
    pool: &BytePool,
) -> (LruState, Option<Atomic<MmapResource>>) {
    let nonempty = fs::metadata(path).is_ok_and(|m| m.len() > 0);
    if !nonempty {
        return (LruState::default(), None);
    }
    match persist::open_existing(path, cancel) {
        Ok(res) => {
            let atomic = Atomic::new(res);
            let initial = read_state(&atomic, pool).unwrap_or_default();
            (initial, Some(atomic))
        }
        Err(e) => {
            tracing::debug!("open existing lru.bin failed: {e}");
            (LruState::default(), None)
        }
    }
}

fn read_state(res: &Atomic<MmapResource>, pool: &BytePool) -> AssetsResult<LruState> {
    let mut buf = pool.get();
    let n = res.read_into(&mut buf)?;

    if n == 0 {
        return Ok(LruState::default());
    }

    let file =
        match rkyv::access::<super::schema::ArchivedLruIndexFile, rkyv::rancor::Error>(&buf[..n]) {
            Ok(archived) => rkyv::deserialize::<LruIndexFile, rkyv::rancor::Error>(archived)
                .expect("LRU archived → owned deserialize"),
            Err(e) => {
                tracing::debug!("Failed to deserialize lru index: {}", e);
                return Ok(LruState::default());
            }
        };

    Ok(LruState::from_file(file))
}

fn write_state(res: &Atomic<MmapResource>, state: &LruState) -> AssetsResult<()> {
    let file = state.to_file();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&file)
        .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
    res.write_all(&bytes)?;
    Ok(())
}

/// In-memory state of the LRU index.
#[derive(Clone, Debug, Default)]
pub(crate) struct LruState {
    clock: u64,
    by_root: HashMap<String, LruEntry>,
}

impl LruState {
    pub(crate) fn len(&self) -> usize {
        self.by_root.len()
    }

    /// Returns `true` if the entry was present and removed.
    pub(crate) fn remove(&mut self, asset_root: &str) -> bool {
        self.by_root.remove(asset_root).is_some()
    }

    /// Touch an asset in-memory.
    pub(crate) fn touch(&mut self, asset_root: &str, bytes_hint: Option<u64>) -> bool {
        self.clock = self.clock.saturating_add(1);

        if let Some(e) = self.by_root.get_mut(asset_root) {
            e.last_touch = self.clock;
            if let Some(b) = bytes_hint {
                e.bytes = Some(b);
            }
            false
        } else {
            self.by_root.insert(
                asset_root.to_string(),
                LruEntry {
                    last_touch: self.clock,
                    bytes: bytes_hint,
                },
            );
            true
        }
    }

    /// Returns `true` if the entry exists and `bytes` actually changed.
    /// Does NOT touch the clock — recency stays exactly where it was.
    pub(crate) fn update_bytes(&mut self, asset_root: &str, bytes: u64) -> bool {
        let Some(entry) = self.by_root.get_mut(asset_root) else {
            return false;
        };
        if entry.bytes == Some(bytes) {
            return false;
        }
        entry.bytes = Some(bytes);
        true
    }

    pub(crate) fn eviction_candidates(
        &self,
        cfg: &EvictConfig,
        pinned: &HashSet<String>,
    ) -> Vec<String> {
        let max_assets = cfg.max_assets;
        let max_bytes = cfg.max_bytes;

        if max_assets.is_none() && max_bytes.is_none() {
            return Vec::new();
        }

        let total_assets = self.len();
        let total_bytes: u64 = self.by_root.values().filter_map(|e| e.bytes).sum();

        if max_assets.is_none_or(|max| total_assets <= max)
            && max_bytes.is_none_or(|max| total_bytes <= max)
        {
            return Vec::new();
        }

        let mut candidates: Vec<_> = self.by_root.iter().collect();
        candidates.sort_by_key(|(_, e)| e.last_touch);

        let mut out = Vec::new();
        let mut simulated_assets = total_assets;
        let mut simulated_bytes = total_bytes;

        for (root, entry) in candidates {
            if pinned.contains(root) {
                continue;
            }

            out.push(root.clone());
            simulated_assets = simulated_assets.saturating_sub(1);
            simulated_bytes = simulated_bytes.saturating_sub(entry.bytes.unwrap_or(0));

            let under_asset_limit = max_assets.is_none_or(|max| simulated_assets <= max);
            let under_byte_limit = max_bytes.is_none_or(|max| simulated_bytes <= max);

            if under_asset_limit && under_byte_limit {
                break;
            }
        }

        out
    }

    fn from_file(file: LruIndexFile) -> Self {
        let mut by_root = HashMap::new();
        for (root, entry) in file.entries {
            by_root.insert(
                root,
                LruEntry {
                    last_touch: entry.last_touch,
                    bytes: entry.bytes,
                },
            );
        }

        Self {
            clock: file.clock,
            by_root,
        }
    }

    fn to_file(&self) -> LruIndexFile {
        let mut entries = BTreeMap::new();
        for (root, entry) in &self.by_root {
            entries.insert(
                root.clone(),
                LruEntryFile {
                    last_touch: entry.last_touch,
                    bytes: entry.bytes,
                },
            );
        }

        LruIndexFile {
            version: 1,
            clock: self.clock,
            entries,
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn update_bytes_does_not_change_eviction_order() {
        // touch(A), touch(B), update_bytes(A) — eviction must still pick A first.
        let lru = LruIndex::ephemeral();
        lru.touch("A", Some(100)).unwrap();
        lru.touch("B", Some(200)).unwrap();
        lru.update_bytes("A", 999).unwrap();

        let cfg = EvictConfig {
            max_assets: Some(1),
            max_bytes: None,
        };
        let candidates = lru.eviction_candidates(&cfg, &HashSet::new());
        assert_eq!(
            candidates,
            vec!["A".to_string()],
            "update_bytes must not bump A past B in LRU order"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn update_bytes_unknown_root_is_noop() {
        let lru = LruIndex::ephemeral();
        let changed = lru.update_bytes("ghost", 4096).unwrap();
        assert!(!changed, "no entry → no change");
        assert_eq!(lru.total_bytes_best_effort(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn update_bytes_idempotent_when_value_matches() {
        let lru = LruIndex::ephemeral();
        lru.touch("A", Some(100)).unwrap();

        let changed = lru.update_bytes("A", 100).unwrap();
        assert!(!changed, "same value → no flush, no change");

        let changed = lru.update_bytes("A", 200).unwrap();
        assert!(changed, "different value → change");
        assert_eq!(lru.total_bytes_best_effort(), 200);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn update_bytes_keeps_clock_unchanged() {
        // Direct LruState assertion — clock must stay frozen across update_bytes.
        let mut state = LruState::default();
        state.touch("A", Some(10));
        state.touch("B", Some(20));
        let clock_before = state.clock;
        let changed = state.update_bytes("A", 999);
        assert!(changed);
        assert_eq!(
            state.clock, clock_before,
            "update_bytes must not bump the LRU clock"
        );
    }
}

#[derive(Clone, Debug)]
struct LruEntry {
    last_touch: u64,
    bytes: Option<u64>,
}
