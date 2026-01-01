use crate::AssetState;

/// Eviction policy for cache assets.
/// Determines which assets should be evicted when space is needed.
pub trait EvictionPolicy {
    /// Sort assets by eviction priority (first = most evictable).
    fn sort_by_priority(&self, assets: &mut Vec<(String, AssetState)>);
}

/// Default LRU (Least Recently Used) eviction policy.
pub struct LruPolicy;

impl EvictionPolicy for LruPolicy {
    fn sort_by_priority(&self, assets: &mut Vec<(String, AssetState)>) {
        // Sort by last_access_ms (oldest first = most evictable)
        assets.sort_by_key(|(_, state)| state.last_access_ms);
    }
}
