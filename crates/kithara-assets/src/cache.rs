#![forbid(unsafe_code)]

use std::{fmt, num::NonZeroUsize, path::Path, sync::Arc};

use kithara_platform::Mutex;
use kithara_storage::{ResourceExt, ResourceStatus};
use lru::LruCache;

use crate::{
    AssetResourceState,
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
type CacheMap<A> = LruCache<
    CacheKey<<A as Assets>::Context>,
    CacheEntry<<A as Assets>::Res, <A as Assets>::IndexRes>,
>;
type CacheItem<A> = (
    CacheKey<<A as Assets>::Context>,
    CacheEntry<<A as Assets>::Res, <A as Assets>::IndexRes>,
);

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
    capacity: NonZeroUsize,
    on_invalidated: Option<crate::store::OnInvalidatedFn>,
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
    pub fn new(
        inner: Arc<A>,
        capacity: NonZeroUsize,
        on_invalidated: Option<crate::store::OnInvalidatedFn>,
    ) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            capacity,
            on_invalidated,
        }
    }

    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }

    fn is_active(&self) -> bool {
        self.inner.capabilities().contains(Capabilities::CACHE)
    }

    fn cached_state(&self, key: &ResourceKey) -> Option<AssetResourceState> {
        let (committed, has_active) = {
            let mut cache = self.cache.lock_sync();
            let mut committed = None;
            let mut has_active = false;
            let mut promote_key = None;

            for (cache_key, entry) in cache.iter() {
                let (CacheKey::Resource(resource_key, _), CacheEntry::Resource(res)) =
                    (cache_key, entry)
                else {
                    continue;
                };

                if resource_key != key {
                    continue;
                }

                match res.status() {
                    ResourceStatus::Failed(reason) => {
                        return Some(AssetResourceState::Failed(reason));
                    }
                    ResourceStatus::Active => has_active = true,
                    ResourceStatus::Committed { final_len } => {
                        if committed.is_none() {
                            committed = Some(AssetResourceState::Committed { final_len });
                            promote_key = Some(cache_key.clone());
                        }
                    }
                }
            }

            // Promote committed entry so it survives until read_from_entry.
            if let Some(pk) = promote_key {
                cache.promote(&pk);
            }
            drop(cache);
            (committed, has_active)
        };

        if has_active {
            return Some(AssetResourceState::Active);
        }

        committed
    }

    fn is_protected_resource(entry: &CacheEntry<A::Res, A::IndexRes>) -> bool {
        matches!(
            entry,
            CacheEntry::Resource(res) if matches!(res.status(), ResourceStatus::Active)
        )
    }

    fn pop_evictable(cache: &mut CacheMap<A>) -> Option<CacheItem<A>> {
        let key = cache
            .iter()
            .filter_map(|(key, entry)| (!Self::is_protected_resource(entry)).then_some(key.clone()))
            .last()?;
        cache.pop(&key).map(|entry| (key, entry))
    }

    fn cache_entry(
        &self,
        cache: &mut CacheMap<A>,
        key: CacheKey<A::Context>,
        entry: CacheEntry<A::Res, A::IndexRes>,
    ) -> Vec<ResourceKey> {
        let mut invalidated = Vec::new();

        while cache.len() >= self.capacity.get() {
            let Some((displaced_key, displaced_entry)) = Self::pop_evictable(cache) else {
                break;
            };
            if let (CacheKey::Resource(resource_key, _), CacheEntry::Resource(_)) =
                (displaced_key, displaced_entry)
            {
                invalidated.push(resource_key);
            }
        }

        let must_cache = Self::is_protected_resource(&entry);

        if cache.len() >= self.capacity.get() && !must_cache {
            return invalidated;
        }

        if must_cache && cache.len() >= self.capacity.get() {
            let overflow_capacity = NonZeroUsize::new(cache.len() + 1)
                .expect("cache overflow capacity must stay non-zero");
            if overflow_capacity > cache.cap() {
                cache.resize(overflow_capacity);
            }
        }

        if let Some((displaced_key, displaced_entry)) = cache.push(key, entry)
            && let (CacheKey::Resource(resource_key, _), CacheEntry::Resource(_)) =
                (displaced_key, displaced_entry)
        {
            invalidated.push(resource_key);
        }

        if cache.len() <= self.capacity.get() && cache.cap() != self.capacity {
            cache.resize(self.capacity);
        }

        invalidated
    }

    /// Compatibility helper for callers that only care about committed resources.
    #[must_use]
    pub fn has_resource(&self, key: &ResourceKey) -> bool {
        matches!(
            self.resource_state(key),
            Ok(AssetResourceState::Committed { .. })
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

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
        if !self.is_active() {
            return self.inner.resource_state(key);
        }

        if let Some(state) = self.cached_state(key) {
            return Ok(state);
        }

        self.inner.resource_state(key)
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

        // Fallback: when ctx=None, look for any committed resource with this key
        if ctx.is_none() {
            if let Some(res) =
                cache
                    .iter()
                    .find_map(|(candidate_key, entry)| match (candidate_key, entry) {
                        (CacheKey::Resource(resource_key, _), CacheEntry::Resource(res))
                            if resource_key == key
                                && matches!(res.status(), ResourceStatus::Committed { .. }) =>
                        {
                            Some(res.clone())
                        }
                        _ => None,
                    })
            {
                return Ok(res);
            }

            if let Some(res) =
                cache
                    .iter()
                    .find_map(|(candidate_key, entry)| match (candidate_key, entry) {
                        (CacheKey::Resource(resource_key, _), CacheEntry::Resource(res))
                            if resource_key == key =>
                        {
                            Some(res.clone())
                        }
                        _ => None,
                    })
            {
                return Ok(res);
            }
        }

        let res = self.inner.open_resource_with_ctx(key, ctx)?;
        let displaced = self.cache_entry(&mut cache, cache_key, CacheEntry::Resource(res.clone()));
        let on_invalidated = self.on_invalidated.clone();
        drop(cache);

        if let Some(cb) = on_invalidated {
            for displaced_key in displaced {
                cb(&displaced_key);
            }
        }

        Ok(res)
    }

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if !self.is_active() {
            return self.inner.acquire_resource_with_ctx(key, ctx);
        }

        let cache_key = CacheKey::Resource(key.clone(), ctx.clone());
        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            res.reactivate()?;
            return Ok(res.clone());
        }

        let res = self.inner.acquire_resource_with_ctx(key, ctx)?;
        let displaced = self.cache_entry(&mut cache, cache_key, CacheEntry::Resource(res.clone()));
        let on_invalidated = self.on_invalidated.clone();
        drop(cache);

        if let Some(cb) = on_invalidated {
            for displaced_key in displaced {
                cb(&displaced_key);
            }
        }

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
        let _ = self.cache_entry(
            &mut cache,
            CacheKey::PinsIndex,
            CacheEntry::Index(res.clone()),
        );
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
        let _ = self.cache_entry(
            &mut cache,
            CacheKey::LruIndex,
            CacheEntry::Index(res.clone()),
        );
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
            let keys_to_remove: Vec<_> = cache
                .iter()
                .filter_map(|(cache_key, _)| match cache_key {
                    CacheKey::Resource(resource_key, _) if resource_key == key => {
                        Some(cache_key.clone())
                    }
                    _ => None,
                })
                .collect();
            for cache_key in keys_to_remove {
                cache.pop(&cache_key);
            }
        }
        self.inner.remove_resource(key)
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{fs, path::Path, sync::Arc, time::Duration};

    use kithara_platform::thread;
    use kithara_storage::{ResourceExt, StorageResource};
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{base::Capabilities, disk_store::DiskAssetStore, mem_store::MemAssetStore};

    #[derive(Clone, Debug)]
    struct ContextMemStore {
        inner: MemAssetStore,
    }

    impl ContextMemStore {
        fn new(asset_root: &str) -> Self {
            Self {
                inner: MemAssetStore::new(asset_root, CancellationToken::new(), None),
            }
        }
    }

    impl Assets for ContextMemStore {
        type Res = StorageResource;
        type Context = u8;
        type IndexRes = kithara_storage::MemResource;

        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }

        fn root_dir(&self) -> &Path {
            self.inner.root_dir()
        }

        fn asset_root(&self) -> &str {
            self.inner.asset_root()
        }

        fn open_resource_with_ctx(
            &self,
            key: &ResourceKey,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<Self::Res> {
            self.inner.open_resource(key)
        }

        fn acquire_resource_with_ctx(
            &self,
            key: &ResourceKey,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<Self::Res> {
            self.inner.acquire_resource(key)
        }

        fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
            self.inner.open_pins_index_resource()
        }

        fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
            self.inner.open_lru_index_resource()
        }

        fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
            self.inner.resource_state(key)
        }

        fn delete_asset(&self) -> AssetsResult<()> {
            self.inner.delete_asset()
        }
    }

    fn make_cached(dir: &Path, capacity: NonZeroUsize) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "test_asset",
            CancellationToken::new(),
        ));
        CachedAssets::new(disk, capacity, None)
    }

    /// Bypass test: empty asset_root → capabilities lack CACHE.
    fn make_cached_disabled(dir: &Path) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(dir, "", CancellationToken::new()));
        CachedAssets::new(disk, NonZeroUsize::new(5).unwrap(), None)
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn evicts_at_custom_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        // Open 4 committed resources — first should be evicted from LRU (capacity 3)
        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            let res = cached.acquire_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
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

        let res1 = cached.acquire_resource(&key).unwrap();
        let res2 = cached.acquire_resource(&key).unwrap();

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
                    c.acquire_resource(&key).unwrap();
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

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn has_resource_matches_contextful_entries() {
        let store = Arc::new(ContextMemStore::new("test_asset"));
        let cached = CachedAssets::new(store, NonZeroUsize::new(4).unwrap(), None);
        let key = ResourceKey::new("segment-0.m4s");

        let res = cached.acquire_resource_with_ctx(&key, Some(7)).unwrap();
        res.write_at(0, b"segment data").unwrap();
        res.commit(Some(12)).unwrap();

        assert!(
            cached.has_resource(&key),
            "committed resource opened with context must still satisfy has_resource(key)"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_without_context_reuses_committed_contextful_resource() {
        let store = Arc::new(ContextMemStore::new("test_asset"));
        let cached = CachedAssets::new(store, NonZeroUsize::new(4).unwrap(), None);
        let key = ResourceKey::new("segment-0.m4s");

        let res = cached.acquire_resource_with_ctx(&key, Some(7)).unwrap();
        res.write_at(0, b"segment data").unwrap();
        res.commit(Some(12)).unwrap();

        let reopened = cached.open_resource(&key).unwrap();
        assert!(
            matches!(reopened.status(), ResourceStatus::Committed { .. }),
            "open_resource(key) must reuse committed contextful entry"
        );

        let mut buf = [0u8; 12];
        let n = reopened.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"segment data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn active_resource_survives_lru_eviction_pressure() {
        let dir = tempfile::tempdir().unwrap();
        let cached = make_cached(dir.path(), NonZeroUsize::new(1).unwrap());
        let key_active = ResourceKey::new("active.m4s");
        let key_other = ResourceKey::new("other.m4s");

        let active = cached.acquire_resource(&key_active).unwrap();
        active.write_at(0, b"abcd").unwrap();

        let other = cached.acquire_resource(&key_other).unwrap();
        other.write_at(0, b"wxyz").unwrap();

        let reopened = cached.open_resource(&key_active).unwrap();
        assert!(
            matches!(reopened.status(), ResourceStatus::Active),
            "active resource must stay discoverable after LRU pressure"
        );
    }
}
