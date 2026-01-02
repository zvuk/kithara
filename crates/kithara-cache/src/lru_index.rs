use crate::{AssetState};
use std::collections::HashMap;

/// LRU indexing utilities for cache eviction.
/// Provides sorting and selection logic for LRU eviction.
pub struct LruIndex;

impl LruIndex {
    /// Sort assets by last access time (oldest first)
    pub fn sort_by_lru(assets: &mut Vec<(String, AssetState)>) {
        assets.sort_by_key(|(_, state)| state.last_access_ms);
    }

    /// Filter out pinned assets
    pub fn filter_unpinned<'a>(
        assets: &'a [(String, AssetState)],
    ) -> impl Iterator<Item = &'a (String, AssetState)> {
        assets.iter().filter(|(_, state)| !state.pinned)
    }

    /// Calculate total bytes from assets
    pub fn total_bytes(assets: &[(String, AssetState)]) -> u64 {
        assets.iter().map(|(_, state)| state.size_bytes).sum()
    }
}