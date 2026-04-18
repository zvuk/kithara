#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};

use kithara_bufpool::BytePool;
use kithara_storage::{Atomic, ResourceExt, StorageError};

use super::schema::{LruEntryFile, LruIndexFile};
use crate::error::{AssetsError, AssetsResult};

/// Eviction configuration for an assets store decorator.
#[derive(Clone, Debug, Default)]
pub struct EvictConfig {
    pub max_assets: Option<usize>,
    pub max_bytes: Option<u64>,
}

/// A best-effort LRU index over `asset_root`.
pub(crate) struct LruIndex<R: ResourceExt> {
    res: Atomic<R>,
    pool: BytePool,
}

impl<R: ResourceExt> LruIndex<R> {
    pub(crate) fn new(res: R, pool: BytePool) -> Self {
        Self {
            res: Atomic::new(res),
            pool,
        }
    }

    /// Load the in-memory state from storage.
    fn load(&self) -> AssetsResult<LruState> {
        let mut buf = self.pool.get();
        let n = self.res.read_into(&mut buf)?;

        if n == 0 {
            return Ok(LruState::default());
        }

        let file = match rkyv::access::<super::schema::ArchivedLruIndexFile, rkyv::rancor::Error>(
            &buf[..n],
        ) {
            Ok(archived) => rkyv::deserialize::<LruIndexFile, rkyv::rancor::Error>(archived)
                .expect("LRU archived → owned deserialize"),
            Err(e) => {
                tracing::debug!("Failed to deserialize lru index: {}", e);
                return Ok(LruState::default());
            }
        };

        Ok(LruState::from_file(file))
    }

    /// Persist the provided state to storage atomically.
    pub(crate) fn store(&self, state: &LruState) -> AssetsResult<()> {
        let file = state.to_file();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&file)
            .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
        self.res.write_all(&bytes)?;
        Ok(())
    }

    /// Touch (mark as most-recent) an asset.
    pub(crate) fn touch(&self, asset_root: &str, bytes_hint: Option<u64>) -> AssetsResult<bool> {
        let mut st = self.load()?;
        let created = st.touch(asset_root, bytes_hint);
        self.store(&st)?;
        Ok(created)
    }

    /// Remove an asset from the index (e.g. after eviction).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remove(&self, asset_root: &str) -> AssetsResult<()> {
        let mut st = self.load()?;
        st.remove(asset_root);
        self.store(&st)?;
        Ok(())
    }

    /// Compute eviction candidates in LRU order (oldest first) under the given config.
    pub(crate) fn eviction_candidates(
        &self,
        cfg: &EvictConfig,
        pinned: &HashSet<String>,
    ) -> AssetsResult<Vec<String>> {
        let st = self.load()?;
        Ok(st.eviction_candidates(cfg, pinned))
    }

    /// Return total bytes across all assets (best-effort).
    pub(crate) fn total_bytes_best_effort(&self) -> u64 {
        if let Ok(st) = self.load() {
            st.by_root.values().filter_map(|e| e.bytes).sum()
        } else {
            0
        }
    }
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

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remove(&mut self, asset_root: &str) {
        self.by_root.remove(asset_root);
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

#[derive(Clone, Debug)]
struct LruEntry {
    last_touch: u64,
    bytes: Option<u64>,
}
