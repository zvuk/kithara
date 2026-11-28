#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};

use super::core::EvictConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::index::schema::{LruEntryFile, LruIndexFile};

/// In-memory state of the LRU index.
#[derive(Clone, Debug, Default)]
pub(super) struct LruState {
    by_root: HashMap<String, LruEntry>,
    clock: u64,
}

impl LruState {
    pub(super) fn total_bytes(&self) -> u64 {
        self.by_root.values().filter_map(|e| e.bytes).sum()
    }

    pub(super) fn eviction_candidates(
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
        let total_bytes = self.total_bytes();

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

    /// Touch an asset in-memory.
    pub(super) fn touch(&mut self, asset_root: &str, bytes_hint: Option<u64>) -> bool {
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
    pub(super) fn update_bytes(&mut self, asset_root: &str, bytes: u64) -> bool {
        let Some(entry) = self.by_root.get_mut(asset_root) else {
            return false;
        };
        if entry.bytes == Some(bytes) {
            return false;
        }
        entry.bytes = Some(bytes);
        true
    }

    delegate::delegate! {
        to self.by_root {
            pub(super) fn len(&self) -> usize;
            /// Returns `true` if the entry was present and removed.
            #[expr($.is_some())]
            pub(super) fn remove(&mut self, asset_root: &str) -> bool;
        }
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

#[derive(Clone, Debug)]
struct LruEntry {
    bytes: Option<u64>,
    last_touch: u64,
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
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
