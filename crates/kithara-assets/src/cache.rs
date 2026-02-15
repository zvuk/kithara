#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, path::Path, sync::Arc};

use lru::LruCache;
use parking_lot::Mutex;

use crate::{base::Assets, error::AssetsResult, key::ResourceKey};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum CacheKey<C> {
    Resource(ResourceKey, Option<C>),
    PinsIndex,
    LruIndex,
    CoverageIndex,
}

#[derive(Clone, Debug)]
enum CacheEntry<R, I> {
    Resource(R),
    Index(I),
}

type Cache<R, C, I> = Mutex<LruCache<CacheKey<C>, CacheEntry<R, I>>>;

/// A decorator that caches opened resources in memory with LRU eviction.
///
/// ## Normative
/// - Caching is done at the resource level (not asset level).
/// - Same `(ResourceKey, Context)` returns the same resource handle.
/// - Cache is process-scoped and not persisted.
/// - LRU capacity is configurable (default: 5 entries).
/// - When `enabled` is `false`, all operations delegate directly to the inner layer.
#[derive(Clone)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    cache: Arc<Cache<A::Res, A::Context, A::IndexRes>>,
    enabled: bool,
    remove_on_evict: bool,
}

impl<A> std::fmt::Debug for CachedAssets<A>
where
    A: Assets,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let size = self.cache.try_lock().map(|c| c.len());
        f.debug_struct("CachedAssets")
            .field("cache_size", &size)
            .finish_non_exhaustive()
    }
}

impl<A> CachedAssets<A>
where
    A: Assets,
{
    pub fn new(inner: Arc<A>, capacity: NonZeroUsize) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            enabled: true,
            remove_on_evict: false,
        }
    }

    /// Create with explicit enabled and remove-on-evict flags.
    ///
    /// When `enabled` is `false`, all operations bypass the cache.
    /// When `remove_on_evict` is `true`, evicted resources are removed from the inner store
    /// via `remove_resource()`. This is useful for ephemeral (in-memory) backends where
    /// eviction from LRU should also free the underlying data.
    pub fn with_options(
        inner: Arc<A>,
        capacity: NonZeroUsize,
        enabled: bool,
        remove_on_evict: bool,
    ) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            enabled,
            remove_on_evict,
        }
    }

    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }
}

impl<A> Assets for CachedAssets<A>
where
    A: Assets,
{
    type Res = A::Res;
    type Context = A::Context;
    type IndexRes = A::IndexRes;

    fn root_dir(&self) -> &Path {
        self.inner.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.inner.asset_root()
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if !self.enabled {
            return self.inner.open_resource_with_ctx(key, ctx);
        }

        let cache_key = CacheKey::Resource(key.clone(), ctx.clone());

        // Hold the lock for the entire check-create-insert sequence.
        // This prevents a TOCTOU race where two threads both miss the cache
        // and create separate MmapResources for the same file.  With
        // OpenMode::Auto the second open sees an existing file and returns a
        // Committed (read-only) resource, making writes fail.
        //
        // The inner chain (Processing → Evict → Disk) does not call back
        // into CachedAssets, so holding the lock is deadlock-free.
        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            return Ok(res.clone());
        }

        let res = self.inner.open_resource_with_ctx(key, ctx)?;
        let evicted = cache.push(cache_key, CacheEntry::Resource(res.clone()));
        // Drop the lock before calling remove_resource to avoid holding it during I/O.
        drop(cache);

        if self.remove_on_evict
            && let Some((CacheKey::Resource(evicted_key, _), _)) = evicted
        {
            let _ = self.inner.remove_resource(&evicted_key);
        }

        Ok(res)
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        if !self.enabled {
            return self.inner.open_pins_index_resource();
        }

        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::PinsIndex) {
            return Ok(res.clone());
        }

        let res = self.inner.open_pins_index_resource()?;
        cache.put(CacheKey::PinsIndex, CacheEntry::Index(res.clone()));
        drop(cache);

        Ok(res)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        if !self.enabled {
            return self.inner.open_lru_index_resource();
        }

        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::LruIndex) {
            return Ok(res.clone());
        }

        let res = self.inner.open_lru_index_resource()?;
        cache.put(CacheKey::LruIndex, CacheEntry::Index(res.clone()));
        drop(cache);

        Ok(res)
    }

    fn open_coverage_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        if !self.enabled {
            return self.inner.open_coverage_index_resource();
        }

        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::CoverageIndex) {
            return Ok(res.clone());
        }

        let res = self.inner.open_coverage_index_resource()?;
        cache.put(CacheKey::CoverageIndex, CacheEntry::Index(res.clone()));
        drop(cache);

        Ok(res)
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        // Clear resource caches for this asset (keep index entries)
        {
            let mut cache = self.cache.lock();

            // Collect keys to remove (LruCache doesn't have retain())
            let keys_to_remove: Vec<CacheKey<A::Context>> = cache
                .iter()
                .filter_map(|(k, _)| {
                    if !matches!(
                        k,
                        CacheKey::<A::Context>::PinsIndex | CacheKey::<A::Context>::LruIndex
                    ) {
                        Some(k.clone())
                    } else {
                        None
                    }
                })
                .collect();

            // Remove collected keys
            for key in keys_to_remove {
                cache.pop(&key);
            }
        }

        self.inner.delete_asset()
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        // Remove from cache (all contexts)
        {
            let mut cache = self.cache.lock();
            let cache_key = CacheKey::Resource(key.clone(), None);
            cache.pop(&cache_key);
        }
        self.inner.remove_resource(key)
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use kithara_storage::ResourceExt;
    use rstest::rstest;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::base::DiskAssetStore;

    fn make_cached(dir: &Path, capacity: NonZeroUsize) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        CachedAssets::new(disk, capacity)
    }

    fn make_cached_disabled(dir: &Path) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        CachedAssets::with_options(disk, NonZeroUsize::new(5).unwrap(), false, false)
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn evicts_at_custom_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        // Open 4 resources — first should be evicted from LRU (capacity 3)
        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            cached.open_resource(key).unwrap();
        }

        // Cache should have exactly 3 entries (capacity)
        let cache = cached.cache.lock();
        assert_eq!(cache.len(), 3);
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn cache_hit_returns_same_resource() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::new("audio.mp3");

        let res1 = cached.open_resource(&key).unwrap();
        let res2 = cached.open_resource(&key).unwrap();

        // Same resource path — cache hit
        assert_eq!(res1.path(), res2.path());
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn concurrent_opens_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = Arc::new(make_cached(dir.path(), cap));

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let c = cached.clone();
                std::thread::spawn(move || {
                    let key = ResourceKey::new(format!("seg_{i}.m4s"));
                    c.open_resource(&key).unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let cache = cached.cache.lock();
        assert_eq!(cache.len(), 4);
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn bypass_does_not_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cached = make_cached_disabled(dir.path());

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            cached.open_resource(key).unwrap();
        }

        // Cache should be empty when disabled
        let cache = cached.cache.lock();
        assert_eq!(cache.len(), 0);
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn bypass_still_returns_resources() {
        let dir = tempfile::tempdir().unwrap();
        let cached = make_cached_disabled(dir.path());
        let key = ResourceKey::new("audio.mp3");

        let res1 = cached.open_resource(&key).unwrap();
        let res2 = cached.open_resource(&key).unwrap();

        // Both should work (same path) even without caching
        assert_eq!(res1.path(), res2.path());
    }

    // ---- remove_on_evict tests ----

    fn make_mem_cached(
        capacity: NonZeroUsize,
        remove_on_evict: bool,
    ) -> (
        Arc<crate::mem_store::MemAssetStore>,
        CachedAssets<crate::mem_store::MemAssetStore>,
    ) {
        let mem = Arc::new(crate::mem_store::MemAssetStore::new(
            "test_asset",
            CancellationToken::new(),
            std::env::temp_dir(),
        ));
        let cached = CachedAssets::with_options(mem.clone(), capacity, true, remove_on_evict);
        (mem, cached)
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn remove_on_evict_false_does_not_remove() {
        let cap = NonZeroUsize::new(2).unwrap();
        let (mem, cached) = make_mem_cached(cap, false);

        let k0 = ResourceKey::new("seg_0.m4s");
        let k1 = ResourceKey::new("seg_1.m4s");
        let k2 = ResourceKey::new("seg_2.m4s");

        cached.open_resource(&k0).unwrap();
        cached.open_resource(&k1).unwrap();
        // This evicts k0 from cache, but should NOT remove from mem store
        cached.open_resource(&k2).unwrap();

        // k0 is still in the inner MemAssetStore (not removed)
        assert!(mem.open_resource_with_ctx(&k0, None).is_ok());
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn remove_on_evict_true_removes_evicted_resource() {
        let cap = NonZeroUsize::new(2).unwrap();
        let (mem, cached) = make_mem_cached(cap, true);

        let k0 = ResourceKey::new("seg_0.m4s");
        let k1 = ResourceKey::new("seg_1.m4s");
        let k2 = ResourceKey::new("seg_2.m4s");

        // Open and write data so we can detect removal
        let res0 = cached.open_resource(&k0).unwrap();
        res0.write_at(0, b"data0").unwrap();
        res0.commit(Some(5)).unwrap();

        cached.open_resource(&k1).unwrap();
        // This evicts k0 from cache AND removes from mem store
        cached.open_resource(&k2).unwrap();

        // k0 should be gone from mem store — re-opening gives a fresh empty resource
        let reopened = mem.open_resource_with_ctx(&k0, None).unwrap();
        assert_eq!(reopened.len(), None, "evicted resource should be gone");
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    fn cache_hit_does_not_trigger_eviction() {
        let cap = NonZeroUsize::new(2).unwrap();
        let (mem, cached) = make_mem_cached(cap, true);

        let k0 = ResourceKey::new("seg_0.m4s");
        let k1 = ResourceKey::new("seg_1.m4s");

        let res0 = cached.open_resource(&k0).unwrap();
        res0.write_at(0, b"data0").unwrap();
        res0.commit(Some(5)).unwrap();

        cached.open_resource(&k1).unwrap();

        // Re-open k0 (cache hit) — no eviction should happen
        cached.open_resource(&k0).unwrap();

        // Both still in mem store
        let r0 = mem.open_resource_with_ctx(&k0, None).unwrap();
        let r1 = mem.open_resource_with_ctx(&k1, None).unwrap();
        assert!(r0.len().is_some(), "k0 should still exist");
        assert!(r1.len().is_none(), "k1 was never committed, len is None");
    }
}
