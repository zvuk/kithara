#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara_platform::sync::{Arc, Mutex};

use super::state::LruState;
use crate::{
    error::AssetsResult,
    index::persistence::{FlushHub, IndexCell, IndexState, flush_sync},
};

/// Eviction configuration for an assets store decorator.
#[derive(Clone, Debug, Default, kithara_config::Config)]
#[config(fields(value))]
pub(crate) struct EvictConfig {
    pub(crate) max_assets: Option<usize>,
    pub(crate) max_bytes: Option<u64>,
}

/// Cloneable shared LRU index over asset roots, with optional best-effort disk persistence.
/// Hydrates an existing file eagerly; mutations flush it, but creation waits for the first flush.
/// The evictor and disk deleter share one instance per cache directory; wasm stays ephemeral.
#[derive(Clone)]
pub(crate) struct LruIndex {
    pub(super) inner: Arc<IndexCell<Mutex<LruState>>>,
}

impl IndexState for Mutex<LruState> {
    const NAME: &'static str = "lru";
}

impl std::fmt::Debug for LruIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("LruIndex");
        dbg.field("len", &self.inner.state.try_lock().map(|s| s.len()).ok());
        dbg.field("persist", &self.inner.is_persisted());
        dbg.finish()
    }
}

impl LruIndex {
    /// Bind this index to a [`FlushHub`] for coordinated flushing.
    /// Called once per index instance; subsequent calls are no-ops.
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        self.inner.attach_to(hub);
    }

    /// Construct an ephemeral index (mem backend mode).
    pub(crate) fn ephemeral() -> Self {
        Self {
            inner: Arc::new(IndexCell::new(Mutex::default())),
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
        self.inner.state.lock().total_bytes()
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

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
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

    #[kithara::test]
    fn update_bytes_unknown_root_is_noop() {
        let lru = LruIndex::ephemeral();
        let changed = lru.update_bytes("ghost", 4096).unwrap();
        assert!(!changed, "no entry → no change");
        assert_eq!(lru.total_bytes_best_effort(), 0);
    }

    #[kithara::test]
    fn update_bytes_idempotent_when_value_matches() {
        let lru = LruIndex::ephemeral();
        lru.touch("A", Some(100)).unwrap();

        let changed = lru.update_bytes("A", 100).unwrap();
        assert!(!changed, "same value → no flush, no change");

        let changed = lru.update_bytes("A", 200).unwrap();
        assert!(changed, "different value → change");
        assert_eq!(lru.total_bytes_best_effort(), 200);
    }
}
