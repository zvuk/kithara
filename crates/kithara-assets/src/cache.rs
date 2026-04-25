#![forbid(unsafe_code)]

use std::{collections::HashSet, fmt, num::NonZeroUsize, path::Path, sync::Arc};

use kithara_platform::Mutex;
use kithara_storage::{ResourceExt, ResourceStatus, StorageResult, WaitOutcome};
use lru::LruCache;

use crate::{
    AssetResourceState,
    base::{Assets, Capabilities},
    error::AssetsResult,
    key::ResourceKey,
};

/// Resource wrapper returned by [`CachedAssets`].
///
/// Delegates all [`ResourceExt`] methods to the inner resource.
/// Adds [`hold`] / [`release`] to pin/unpin the resource in the
/// LRU cache so it is never evicted.
#[derive(Clone)]
pub struct CachedResource<R> {
    inner: R,
    key: ResourceKey,
    pinned: Arc<Mutex<HashSet<ResourceKey>>>,
}

impl<R: fmt::Debug> fmt::Debug for CachedResource<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<R> CachedResource<R> {
    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.pinned.lock_sync().insert(self.key.clone());
    }

    /// Unpin this resource (by-ref, for use inside wrappers).
    pub(crate) fn set_released(&self) {
        self.pinned.lock_sync().remove(&self.key);
    }

    /// Pin this resource in the LRU cache. It will not be evicted
    /// until [`release`] is called for the same key.
    pub fn retain(self) -> Self {
        self.set_retained();
        self
    }

    /// Unpin this resource, making it eligible for LRU eviction.
    pub fn release(self) -> Self {
        self.set_released();
        self
    }
}

impl<R: ResourceExt + Clone + Send + Sync + fmt::Debug + 'static> ResourceExt
    for CachedResource<R>
{
    delegate::delegate! {
        to self.inner {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
            fn wait_range(&self, range: std::ops::Range<u64>) -> StorageResult<WaitOutcome>;
            fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;
            fn fail(&self, reason: String);
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn reactivate(&self) -> StorageResult<()>;
            fn contains_range(&self, range: std::ops::Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<std::ops::Range<u64>>;
        }
    }
}

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
type SharedCache<A> =
    Arc<Cache<<A as Assets>::Res, <A as Assets>::Context, <A as Assets>::IndexRes>>;
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
/// - Resources can be pinned via [`CachedResource::hold`] / [`CachedResource::release`].
///   Pinned resources live outside the target capacity and are never evicted.
/// - When the inner store lacks [`Capabilities::CACHE`], all operations
///   delegate directly to the inner layer.
#[derive(Clone)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    cache: SharedCache<A>,
    pinned: Arc<Mutex<HashSet<ResourceKey>>>,
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
            pinned: Arc::new(Mutex::new(HashSet::new())),
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

    fn wrap(&self, key: &ResourceKey, inner: A::Res) -> CachedResource<A::Res> {
        CachedResource {
            inner,
            key: key.clone(),
            pinned: Arc::clone(&self.pinned),
        }
    }

    fn open_index_resource<F>(
        &self,
        cache_key: CacheKey<A::Context>,
        load: F,
    ) -> AssetsResult<A::IndexRes>
    where
        F: FnOnce() -> AssetsResult<A::IndexRes>,
    {
        if !self.is_active() {
            return load();
        }

        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Index(res)) = cache.peek(&cache_key) {
            return Ok(res.clone());
        }

        let res = load()?;
        let _ = self.cache_entry(&mut cache, cache_key, CacheEntry::Index(res.clone()));
        drop(cache);

        Ok(res)
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
                    // Cancelled folds into Active for the same reason
                    // `AssetResourceState::From` does — a cancelled
                    // handle still represents a live cache entry
                    // whose partial bytes are valid for a Phase-2
                    // reopen. The cancellation signal is consumed by
                    // `ProcessedResource::inner_terminal` via
                    // `ResourceStatus::Cancelled` directly.
                    ResourceStatus::Active | ResourceStatus::Cancelled => has_active = true,
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

    fn is_pinned_key(&self, key: &CacheKey<A::Context>) -> bool {
        match key {
            CacheKey::Resource(resource_key, _) => self.pinned.lock_sync().contains(resource_key),
            _ => false,
        }
    }

    fn pop_evictable(&self, cache: &mut CacheMap<A>) -> Option<CacheItem<A>> {
        let key = cache
            .iter()
            .filter_map(|(key, entry)| {
                if Self::is_protected_resource(entry) || self.is_pinned_key(key) {
                    return None;
                }
                Some(key.clone())
            })
            .last()?;
        cache.pop(&key).map(|entry| (key, entry))
    }

    fn pinned_cache_count(&self, cache: &CacheMap<A>) -> usize {
        let pinned = self.pinned.lock_sync();
        cache
            .iter()
            .filter(|(k, e)| {
                Self::is_protected_resource(e)
                    || matches!(k, CacheKey::Resource(rk, _) if pinned.contains(rk))
            })
            .count()
    }

    fn cache_entry(
        &self,
        cache: &mut CacheMap<A>,
        key: CacheKey<A::Context>,
        entry: CacheEntry<A::Res, A::IndexRes>,
    ) -> Vec<ResourceKey> {
        let mut invalidated = Vec::new();

        let effective = self.capacity.get() + self.pinned_cache_count(cache);

        while cache.len() >= effective {
            let Some((displaced_key, displaced_entry)) = self.pop_evictable(cache) else {
                break;
            };
            if let (CacheKey::Resource(resource_key, _), CacheEntry::Resource(_)) =
                (displaced_key, displaced_entry)
            {
                invalidated.push(resource_key);
            }
        }

        // Grow underlying LRU storage when pinned entries push the
        // actual count beyond `lru::LruCache::cap()`.
        if cache.len() >= cache.cap().get() {
            let grow = NonZeroUsize::new(cache.len() + 1)
                .expect("cache overflow capacity must stay non-zero");
            cache.resize(grow);
        }

        if let Some((displaced_key, displaced_entry)) = cache.push(key, entry)
            && let (CacheKey::Resource(resource_key, _), CacheEntry::Resource(_)) =
                (displaced_key, displaced_entry)
        {
            invalidated.push(resource_key);
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
    type Res = CachedResource<A::Res>;
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
            return Ok(self.wrap(key, self.inner.open_resource_with_ctx(key, ctx)?));
        }

        let cache_key = CacheKey::Resource(key.clone(), ctx.clone());

        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            return Ok(self.wrap(key, res.clone()));
        }

        if ctx.is_none() {
            // Read-path (`open_resource`) must only reuse *committed* cache
            // entries carrying a different context (e.g. DRM writer committed
            // the resource with `ctx=Some(..)`; reader arrives with
            // `ctx=None`). Falling back to any entry — including an
            // uncommitted one still being streamed by a ctx=Some writer —
            // would surface the `ProcessedResource` pre-commit read guard
            // ("processed resource is not readable before commit") as a hard
            // error in the reader path, poisoning the decoder FSM.
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
                return Ok(self.wrap(key, res));
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

        Ok(self.wrap(key, res))
    }

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if !self.is_active() {
            return Ok(self.wrap(key, self.inner.acquire_resource_with_ctx(key, ctx)?));
        }

        let cache_key = CacheKey::Resource(key.clone(), ctx.clone());
        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            // Cache-hit on an already-committed resource must NOT
            // reactivate: reactivation flips `processed=false` /
            // `committed=false` on the SHARED `ProcessedResource`,
            // which poisons any concurrent reader holding a cloned Arc
            // (reader.read_at fires "processed resource is not readable
            // before commit" as a hard StorageError::Failed → decoder
            // FSM → Failed). The already-committed data is exactly what
            // the caller wants: just return a clone. See
            // red_test_drm_small_cache_writer_reactivate_poisons_concurrent_reader.
            //
            // Only reactivate when the cached resource is not
            // Committed — that's the legitimate "LRU slot reuse" case.
            let res = res.clone();
            if !matches!(res.status(), ResourceStatus::Committed { .. }) {
                res.reactivate()?;
            }
            drop(cache);
            return Ok(self.wrap(key, res));
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

        Ok(self.wrap(key, res))
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.open_index_resource(CacheKey::PinsIndex, || {
            self.inner.open_pins_index_resource()
        })
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.open_index_resource(CacheKey::LruIndex, || self.inner.open_lru_index_resource())
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
        // Also unpin when removing
        self.pinned.lock_sync().remove(key);
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
                inner: MemAssetStore::new(
                    asset_root,
                    CancellationToken::new(),
                    None,
                    crate::byte_pool(),
                ),
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
            crate::byte_pool(),
        ));
        CachedAssets::new(disk, capacity, None)
    }

    /// Bypass test: empty `asset_root` → capabilities lack CACHE.
    fn make_cached_disabled(dir: &Path) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            "",
            CancellationToken::new(),
            crate::byte_pool(),
        ));
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
    fn pinned_resources_survive_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..5)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        // Acquire and pin the first resource via retain()
        let first = cached.acquire_resource(&keys[0]).unwrap().retain();
        first.write_at(0, b"data").unwrap();
        first.commit(Some(4)).unwrap();

        for key in &keys[1..] {
            let res = cached.acquire_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // Pinned key should survive: 3 normal + 1 pinned = 4
        assert_eq!(cached.cache.lock_sync().len(), 4);
        assert!(
            cached.has_resource(&keys[0]),
            "pinned resource must survive"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn release_unpins_and_allows_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..5)
            .map(|i| ResourceKey::new(format!("seg_{i}.m4s")))
            .collect();

        // Pin the first resource
        let first = cached.acquire_resource(&keys[0]).unwrap().retain();
        first.write_at(0, b"data").unwrap();
        first.commit(Some(4)).unwrap();

        // Fill up to capacity + 1 pinned
        for key in &keys[1..4] {
            let res = cached.acquire_resource(key).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }
        assert_eq!(cached.cache.lock_sync().len(), 4, "3 normal + 1 pinned");

        // Unpin the first resource
        first.release();

        // Add one more — the previously-pinned resource should now be evictable
        let res = cached.acquire_resource(&keys[4]).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        // Cache should shrink back to base capacity
        assert_eq!(
            cached.cache.lock_sync().len(),
            3,
            "released resource must be evicted, cache back to capacity"
        );
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
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::new("audio.mp3");

        let res = cached.acquire_resource(&key).unwrap();
        res.write_at(0, b"hello").unwrap();
        res.commit(Some(5)).unwrap();

        let read_res = cached.open_resource(&key).unwrap();
        let mut buf = [0u8; 5];
        read_res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn delete_asset_clears_resource_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);

        let key = ResourceKey::new("delete_me.mp3");
        let _res = cached.acquire_resource(&key).unwrap();

        assert!(!cached.cache.lock_sync().is_empty());

        cached.delete_asset().unwrap();

        // delete_asset now clears both index and resource entries
        assert_eq!(cached.cache.lock_sync().len(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn remove_resource_clears_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);

        let key = ResourceKey::new("remove_me.mp3");
        let res = cached.acquire_resource(&key).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        // Sanity: resource exists
        assert!(cached.has_resource(&key));

        // Trigger removal — clears cache + unlinks from backend
        cached.remove_resource(&key).unwrap();

        // After removal: resource_state should no longer report Committed
        let state = cached.resource_state(&key).unwrap();
        assert_eq!(state, AssetResourceState::Missing);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn bypass_when_not_active() {
        let dir = tempfile::tempdir().unwrap();
        let cached = make_cached_disabled(dir.path());

        let key = ResourceKey::new("bypass.mp3");

        // Cache inactive — should still open via inner store
        assert!(cached.acquire_resource(&key).is_err());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_returns_committed_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::new("committed.m4s");

        // Write + commit
        let res = cached.acquire_resource(&key).unwrap();
        res.write_at(0, b"hello").unwrap();
        res.commit(Some(5)).unwrap();

        // open_resource should find the committed entry
        let opened = cached.open_resource(&key).unwrap();
        let mut buf = [0u8; 5];
        opened.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn context_aware_caching_separates_keys() {
        let store = ContextMemStore::new("ctx_test");
        let cached = CachedAssets::new(Arc::new(store), NonZeroUsize::new(5).unwrap(), None);
        let key = ResourceKey::new("seg.m4s");

        // Two different contexts for same key yield separate resources
        let r1 = cached.acquire_resource_with_ctx(&key, Some(1)).unwrap();
        r1.write_at(0, b"aaa").unwrap();
        r1.commit(Some(3)).unwrap();

        let r2 = cached.acquire_resource_with_ctx(&key, Some(2)).unwrap();
        r2.write_at(0, b"bbb").unwrap();
        r2.commit(Some(3)).unwrap();

        // Reading back should return the correct resource per context
        let check1 = cached.open_resource_with_ctx(&key, Some(1)).unwrap();
        let check2 = cached.open_resource_with_ctx(&key, Some(2)).unwrap();
        let mut b1 = [0u8; 3];
        let mut b2 = [0u8; 3];
        check1.read_at(0, &mut b1).unwrap();
        check2.read_at(0, &mut b2).unwrap();
        assert_eq!(&b1, b"aaa");
        assert_eq!(&b2, b"bbb");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn disk_lru_keeps_committed_data_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(2).unwrap();
        let cached = make_cached(dir.path(), cap);

        // Resource A
        let key_a = ResourceKey::new("a.m4s");
        let a = cached.acquire_resource(&key_a).unwrap();
        a.write_at(0, b"aaaa").unwrap();
        a.commit(Some(4)).unwrap();

        // Resource B
        let key_b = ResourceKey::new("b.m4s");
        let b = cached.acquire_resource(&key_b).unwrap();
        b.write_at(0, b"bbbb").unwrap();
        b.commit(Some(4)).unwrap();

        // Resource C — evicts A from LRU
        let key_c = ResourceKey::new("c.m4s");
        let c = cached.acquire_resource(&key_c).unwrap();
        c.write_at(0, b"cccc").unwrap();
        c.commit(Some(4)).unwrap();

        // A is evicted from LRU but its data is still on disk (DiskAssetStore)
        let a_path = dir.path().join("test_asset").join("a.m4s");
        assert!(a_path.exists(), "committed data must remain on disk");
        assert_eq!(fs::read(&a_path).unwrap(), b"aaaa");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_with_none_context_finds_committed() {
        let store = ContextMemStore::new("ctx_test2");
        let cached = CachedAssets::new(Arc::new(store), NonZeroUsize::new(5).unwrap(), None);
        let key = ResourceKey::new("seg.m4s");

        // Commit with a specific context
        let res = cached.acquire_resource_with_ctx(&key, Some(42)).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        // Opening with None should find the committed resource
        let found = cached.open_resource(&key).unwrap();
        let mut buf = [0u8; 4];
        found.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn concurrent_acquire_returns_same_resource() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = Arc::new(make_cached(dir.path(), cap));

        let key = ResourceKey::new("concurrent.m4s");
        let key2 = key.clone();
        let cached2 = Arc::clone(&cached);

        let h = thread::spawn(move || {
            let _res = cached2.acquire_resource(&key2).unwrap();
        });

        let _res = cached.acquire_resource(&key).unwrap();
        h.join().unwrap();

        // Both threads opened the same logical resource
        assert_eq!(cached.cache.lock_sync().len(), 1);
    }
}
