#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use kithara_bufpool::BytePool;
use kithara_storage::{Atomic, ResourceExt};

use crate::error::AssetsResult;

/// Eviction configuration for an assets store decorator.
///
/// ## Normative
/// - Eviction is evaluated at "asset creation time" (i.e. when a new `asset_root` is observed),
///   not as a background task.
/// - `max_assets` and `max_bytes` are soft caps enforced best-effort by the eviction decorator.
/// - If a candidate is pinned, it must not be evicted and is skipped.
/// - If `max_assets` / `max_bytes` are `None`, that constraint is disabled.
///
/// Notes:
/// - `max_bytes` is based on best-effort accounting in the LRU index (not a filesystem walk).
#[derive(Clone, Debug, Default)]
pub struct EvictConfig {
    pub max_assets: Option<usize>,
    pub max_bytes: Option<u64>,
}

/// A best-effort, storage-backed LRU index over `asset_root`.
///
/// This index is persisted via a resource implementing [`ResourceExt`]
/// (whole-object read/write).
///
/// ## Data model (normative)
/// - We track per-asset metadata:
///   - `last_touch`: monotonically increasing counter (logical clock)
///   - `bytes`: best-effort size accounting provided by higher layers (or left as `None`)
/// - The on-disk representation is internal to this module (binary format via bincode).
///
/// ## What this is NOT
/// - It is not a filesystem walker.
/// - It does not delete anything.
/// - It does not know about pinning; the eviction decorator combines this with a pins index.
pub struct LruIndex<R: ResourceExt> {
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

    /// Load the entire LRU table from storage.
    ///
    /// Missing/empty file is treated as an empty index.
    /// Invalid or corrupted data is treated as an empty index (best-effort).
    pub fn load(&self) -> AssetsResult<LruState> {
        let mut buf = self.pool.get();
        let n = self.res.read_into(&mut buf)?;

        if n == 0 {
            return Ok(LruState::default());
        }

        let file: LruIndexFile =
            match bincode::serde::decode_from_slice(&buf, bincode::config::legacy()) {
                Ok((file, _)) => file,
                Err(_) => return Ok(LruState::default()),
            };

        Ok(LruState::from_file(file))
    }

    /// Persist the provided state to storage atomically.
    pub fn store(&self, state: &LruState) -> AssetsResult<()> {
        let file = state.to_file();
        let bytes = bincode::serde::encode_to_vec(&file, bincode::config::legacy())?;
        self.res.write_all(&bytes)?;
        Ok(())
    }

    /// Touch (mark as most-recent) an asset.
    ///
    /// - If the asset is new, it is inserted.
    /// - If `bytes_hint` is `Some`, the stored bytes are updated to that value.
    /// - Returns `true` if the asset was newly inserted (i.e. "created").
    pub fn touch(&self, asset_root: &str, bytes_hint: Option<u64>) -> AssetsResult<bool> {
        let mut st = self.load()?;
        let created = st.touch(asset_root, bytes_hint);
        self.store(&st)?;
        Ok(created)
    }

    /// Remove an asset from the index (e.g. after eviction).
    pub fn remove(&self, asset_root: &str) -> AssetsResult<()> {
        let mut st = self.load()?;
        st.remove(asset_root);
        self.store(&st)?;
        Ok(())
    }

    /// Compute eviction candidates in LRU order (oldest first) under the given config.
    ///
    /// - `pinned` contains `asset_root`s that must not be evicted (will be skipped).
    /// - Returns a list of candidate `asset_root`s to attempt eviction for.
    ///
    /// This function does not mutate storage.
    pub fn eviction_candidates(
        &self,
        cfg: &EvictConfig,
        pinned: &HashSet<String>,
    ) -> AssetsResult<Vec<String>> {
        let st = self.load()?;
        Ok(st.eviction_candidates(cfg, pinned))
    }
}

/// In-memory state of the LRU index.
///
/// This is separated to:
/// - keep persistence format private,
/// - keep logic testable without storage.
#[derive(Clone, Debug, Default)]
pub struct LruState {
    clock: u64,
    by_root: HashMap<String, LruEntry>,
}

impl LruState {
    pub fn len(&self) -> usize {
        self.by_root.len()
    }

    pub fn remove(&mut self, asset_root: &str) {
        self.by_root.remove(asset_root);
    }

    /// Touch an asset in-memory.
    ///
    /// Returns `true` if it was newly inserted.
    pub fn touch(&mut self, asset_root: &str, bytes_hint: Option<u64>) -> bool {
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

    pub fn total_bytes_best_effort(&self) -> u64 {
        self.by_root
            .values()
            .filter_map(|e| e.bytes)
            .fold(0u64, u64::saturating_add)
    }

    pub fn oldest_first(&self) -> Vec<(String, LruEntry)> {
        let mut v: Vec<(String, LruEntry)> = self
            .by_root
            .iter()
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect();

        v.sort_by_key(|(_k, e)| e.last_touch);
        v
    }

    pub fn eviction_candidates(&self, cfg: &EvictConfig, pinned: &HashSet<String>) -> Vec<String> {
        let max_assets = cfg.max_assets;
        let max_bytes = cfg.max_bytes;

        // Fast path: no constraints.
        if max_assets.is_none() && max_bytes.is_none() {
            return Vec::new();
        }

        let mut over_assets = false;
        if let Some(max) = max_assets {
            over_assets = self.len() > max;
        }

        let mut over_bytes = false;
        let total_bytes = self.total_bytes_best_effort();
        if let Some(max) = max_bytes {
            over_bytes = total_bytes > max;
        }

        if !over_assets && !over_bytes {
            return Vec::new();
        }

        // We propose eviction in oldest-first order, skipping pinned.
        // We stop when both constraints are satisfied under a simulated removal.
        let mut simulated_assets = self.len();
        let mut simulated_bytes = total_bytes;

        let mut out = Vec::new();

        for (root, entry) in self.oldest_first() {
            if pinned.contains(&root) {
                continue;
            }

            // Propose evicting this asset.
            out.push(root.clone());

            simulated_assets = simulated_assets.saturating_sub(1);
            if let Some(b) = entry.bytes {
                simulated_bytes = simulated_bytes.saturating_sub(b);
            }

            // Stop if both constraints are satisfied.
            let satisfied_assets = max_assets.is_none_or(|max| simulated_assets <= max);
            let satisfied_bytes = max_bytes.is_none_or(|max| simulated_bytes <= max);
            if satisfied_assets && satisfied_bytes {
                break;
            }
        }

        out
    }

    fn from_file(file: LruIndexFile) -> Self {
        let mut by_root = HashMap::with_capacity(file.entries.len());
        for e in file.entries {
            by_root.insert(
                e.asset_root,
                LruEntry {
                    last_touch: e.last_touch,
                    bytes: e.bytes,
                },
            );
        }

        Self {
            clock: file.clock,
            by_root,
        }
    }

    fn to_file(&self) -> LruIndexFile {
        let mut entries: Vec<LruIndexFileEntry> = self
            .by_root
            .iter()
            .map(|(root, e)| LruIndexFileEntry {
                asset_root: root.clone(),
                last_touch: e.last_touch,
                bytes: e.bytes,
            })
            .collect();

        // Stable output: sort by asset_root for deterministic serialization.
        entries.sort_by(|a, b| a.asset_root.cmp(&b.asset_root));

        LruIndexFile {
            version: 1,
            clock: self.clock,
            entries,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LruEntry {
    pub last_touch: u64,
    pub bytes: Option<u64>,
}

/// On-disk binary format (private).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct LruIndexFile {
    version: u32,
    clock: u64,
    entries: Vec<LruIndexFileEntry>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct LruIndexFileEntry {
    asset_root: String,
    last_touch: u64,
    bytes: Option<u64>,
}
