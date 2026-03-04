#![forbid(unsafe_code)]

use std::{fmt, num::NonZeroUsize, path::Path, sync::Arc};

use kithara_platform::Mutex;
use kithara_storage::{ResourceExt, ResourceStatus};
use lru::LruCache;

use crate::{
    base::{Assets, Capabilities},
    error::AssetsResult,
    key::ResourceKey,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum CacheKey<C> {
    Resource(ResourceKey, Option<C>),
    PinsIndex,
    LruIndex,
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
/// - When the inner store lacks [`Capabilities::CACHE`], all operations
///   delegate directly to the inner layer.
#[derive(Clone)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    cache: Arc<Cache<A::Res, A::Context, A::IndexRes>>,
}

impl<A> fmt::Debug for CachedAssets<A>
where
    A: Assets,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = self.cache.try_lock().ok().map(|c| c.len());
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
        }
    }

    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }

    fn is_active(&self) -> bool {
        self.inner.capabilities().contains(Capabilities::CACHE)
    }

    /// Check whether a committed resource is currently in the LRU cache.
    ///
    /// Uses `peek` (no LRU promotion) to avoid side effects.
    /// Returns `false` for `Active` (empty) placeholders created after eviction.
    /// Returns `true` for non-cached backends (disk always has resources).
    #[must_use]
    pub fn has_resource(&self, key: &ResourceKey) -> bool {
        if !self.is_active() {
            return true;
        }
        let cache_key = CacheKey::Resource(key.clone(), None);
        matches!(
            self.cache.lock_sync().peek(&cache_key),
            Some(CacheEntry::Resource(res))
                if matches!(res.status(), ResourceStatus::Committed { .. })
        )
    }
}

impl<A> Assets for CachedAssets<A>
where
    A: Assets,
{
    type Res = A::Res;
    type Context = A::Context;
    type IndexRes = A::IndexRes;

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> Capabilities;
            fn root_dir(&self) -> &Path;
            fn asset_root(&self) -> &str;
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if !self.is_active() {
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
        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            return Ok(res.clone());
        }

        let res = self.inner.open_resource_with_ctx(key, ctx)?;
        cache.push(cache_key, CacheEntry::Resource(res.clone()));
        drop(cache);

        Ok(res)
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        if !self.is_active() {
            return self.inner.open_pins_index_resource();
        }

        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::PinsIndex) {
            return Ok(res.clone());
        }

        let res = self.inner.open_pins_index_resource()?;
        cache.put(CacheKey::PinsIndex, CacheEntry::Index(res.clone()));
        drop(cache);

        Ok(res)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        if !self.is_active() {
            return self.inner.open_lru_index_resource();
        }

        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::LruIndex) {
            return Ok(res.clone());
        }

        let res = self.inner.open_lru_index_resource()?;
        cache.put(CacheKey::LruIndex, CacheEntry::Index(res.clone()));
        drop(cache);

        Ok(res)
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        // Clear resource caches for this asset (keep index entries)
        {
            let mut cache = self.cache.lock_sync();

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
            let mut cache = self.cache.lock_sync();
            let cache_key = CacheKey::Resource(key.clone(), None);
            cache.pop(&cache_key);
        }
        self.inner.remove_resource(key)
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{fs, sync::Arc, time::Duration};

    use kithara_platform::thread;
    use kithara_storage::ResourceExt;
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::disk_store::DiskAssetStore;

    fn make_cached(dir: &Path, capacity: NonZeroUsize) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        CachedAssets::new(disk, capacity)
    }

    /// Bypass test: empty asset_root → capabilities lack CACHE.
    fn make_cached_disabled(dir: &Path) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(dir, "", CancellationToken::new()));
        CachedAssets::new(disk, NonZeroUsize::new(5).unwrap())
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
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
        assert_eq!(cached.cache.lock_sync().len(), 3);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn concurrent_opens_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = Arc::new(make_cached(dir.path(), cap));

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let c = cached.clone();
                thread::spawn(move || {
                    let key = ResourceKey::new(format!("seg_{i}.m4s"));
                    c.open_resource(&key).unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(cached.cache.lock_sync().len(), 4);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_does_not_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cached = make_cached_disabled(dir.path());

        // absolute keys because asset_root is empty
        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| {
                let p = dir.path().join(format!("seg_{i}.m4s"));
                fs::write(&p, b"data").unwrap();
                ResourceKey::absolute(&p)
            })
            .collect();

        for key in &keys {
            cached.open_resource(key).unwrap();
        }

        // Cache should be empty when capability absent
        assert_eq!(cached.cache.lock_sync().len(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_still_returns_resources() {
        let dir = tempfile::tempdir().unwrap();
        let cached = make_cached_disabled(dir.path());
        let p = dir.path().join("audio.mp3");
        fs::write(&p, b"data").unwrap();
        let key = ResourceKey::absolute(&p);

        let res1 = cached.open_resource(&key).unwrap();
        let res2 = cached.open_resource(&key).unwrap();

        // Both should work (same path) even without caching
        assert_eq!(res1.path(), res2.path());
    }
}
