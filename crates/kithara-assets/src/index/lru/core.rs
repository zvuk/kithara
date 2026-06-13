#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock, Weak, atomic::AtomicBool},
};

use kithara_platform::Mutex;

#[cfg(not(target_arch = "wasm32"))]
use crate::index::schema::{LruEntryFile, LruIndexFile};
use crate::{
    error::AssetsResult,
    flush::{FlushHub, Flushable, flush_sync},
};

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
/// run is opened eagerly during `Self::with_persist_at` (native only)
/// for hydration. On wasm32 the index is always ephemeral.
///
/// Two call-sites share a single instance per `cache_dir`:
///   * `EvictAssets` (touch/remove during open and eviction),
///   * `DiskAssetDeleter` (drop entry when an `asset_root` is fully removed).
#[derive(Clone)]
pub(crate) struct LruIndex {
    pub(super) inner: Arc<LruInner>,
}

impl std::fmt::Debug for LruIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("LruIndex");
        dbg.field("len", &self.inner.state.try_lock().map(|s| s.len()).ok());
        #[cfg(not(target_arch = "wasm32"))]
        dbg.field("persist", &self.inner.persist.is_some());
        dbg.finish()
    }
}

pub(super) struct LruInner {
    pub(super) dirty: AtomicBool,
    pub(super) state: Mutex<LruState>,
    /// Set by [`LruIndex::attach_to`]. While `None`, mutators flush
    /// inline through [`Flushable::flush`] — matches the historical
    /// inline-flush behaviour for ad-hoc tests.
    pub(super) hub: OnceLock<Arc<FlushHub>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) persist: Option<super::disk::LruPersist>,
}

impl LruIndex {
    /// Bind this index to a [`FlushHub`] for coordinated flushing.
    /// Called once per index instance; subsequent calls are no-ops.
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        if self.inner.hub.set(Arc::clone(hub)).is_err() {
            return;
        }
        hub.register(Arc::downgrade(&self.inner) as Weak<dyn Flushable>);
    }

    /// Construct an ephemeral index (mem backend mode).
    pub(crate) fn ephemeral() -> Self {
        Self {
            inner: Arc::new(LruInner {
                state: Mutex::default(),
                #[cfg(not(target_arch = "wasm32"))]
                persist: None,
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
            }),
        }
    }

    /// Compute eviction candidates in LRU order (oldest first) under
    /// the given config. Pure in-memory read — never fails.
    pub(crate) fn eviction_candidates(
        &self,
        cfg: &EvictConfig,
        pinned: &HashSet<String>,
    ) -> Vec<String> {
        let st = self.inner.state.lock();
        st.eviction_candidates(cfg, pinned)
    }

    /// Remove an asset from the index (eviction or full deletion).
    ///
    /// Same durability contract as [`Self::touch`].
    ///
    /// # Errors
    ///
    /// Propagates [`AssetsError`](crate::error::AssetsError) when the
    /// on-disk flush fails.
    pub(crate) fn remove(&self, asset_root: &str) -> AssetsResult<()> {
        let removed = {
            let mut st = self.inner.state.lock();
            st.remove(asset_root)
        };
        if removed {
            flush_sync(&*self.inner)?;
        }
        Ok(())
    }

    /// Return total bytes across all assets (best-effort).
    pub(crate) fn total_bytes_best_effort(&self) -> u64 {
        self.inner
            .state
            .lock()
            .by_root
            .values()
            .filter_map(|e| e.bytes)
            .sum()
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
    /// Propagates [`AssetsError`](crate::error::AssetsError) when the
    /// on-disk flush fails.
    pub(crate) fn touch(&self, asset_root: &str, bytes_hint: Option<u64>) -> AssetsResult<bool> {
        let created = {
            let mut st = self.inner.state.lock();
            st.touch(asset_root, bytes_hint)
        };
        flush_sync(&*self.inner)?;
        Ok(created)
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
    /// Propagates [`AssetsError`](crate::error::AssetsError) when the
    /// on-disk flush fails.
    pub(crate) fn update_bytes(&self, asset_root: &str, bytes: u64) -> AssetsResult<bool> {
        let changed = {
            let mut st = self.inner.state.lock();
            st.update_bytes(asset_root, bytes)
        };
        if changed {
            flush_sync(&*self.inner)?;
        }
        Ok(changed)
    }
}

impl Flushable for LruInner {
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
        "lru"
    }
}

#[cfg(target_arch = "wasm32")]
impl LruInner {
    fn flush_with_durability(&self, _durable: bool) -> AssetsResult<()> {
        self.dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// In-memory state of the LRU index.
#[derive(Clone, Debug, Default)]
pub(crate) struct LruState {
    by_root: HashMap<String, LruEntry>,
    clock: u64,
}

impl LruState {
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

        let within_limits = |assets: usize, bytes: u64| {
            max_assets.is_none_or(|max| assets <= max) && max_bytes.is_none_or(|max| bytes <= max)
        };

        candidates
            .into_iter()
            .filter(|(root, _)| !pinned.contains(*root))
            .scan(
                (total_assets, total_bytes, false),
                |(assets, bytes, done), (root, entry)| {
                    if *done {
                        return None;
                    }
                    *assets = assets.saturating_sub(1);
                    *bytes = bytes.saturating_sub(entry.bytes.unwrap_or(0));
                    *done = within_limits(*assets, *bytes);
                    Some(root.clone())
                },
            )
            .collect()
    }

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
            if bytes_hint.is_some() {
                e.bytes = bytes_hint;
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
}

#[cfg(not(target_arch = "wasm32"))]
impl From<&LruState> for LruIndexFile {
    fn from(state: &LruState) -> Self {
        let mut entries = BTreeMap::new();
        for (root, entry) in &state.by_root {
            entries.insert(
                root.clone(),
                LruEntryFile {
                    last_touch: entry.last_touch,
                    bytes: entry.bytes,
                },
            );
        }
        Self {
            entries,
            version: 1,
            clock: state.clock,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<LruIndexFile> for LruState {
    fn from(file: LruIndexFile) -> Self {
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
            by_root,
            clock: file.clock,
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
    bytes: Option<u64>,
    last_touch: u64,
}
