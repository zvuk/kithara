#![forbid(unsafe_code)]

use std::{fmt, num::NonZeroUsize, path::Path, sync::Arc};

use dashmap::DashSet;
use kithara_platform::Mutex;
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};
use lru::LruCache;

use crate::{
    AssetResourceState,
    base::{Assets, Capabilities, ResourceHandle},
    error::AssetsResult,
    identity::RequestIdentity,
    key::ResourceKey,
};

/// Resource wrapper returned by [`CachedAssets`].
#[derive(Clone)]
pub struct CachedResource<R> {
    pinned: Arc<DashSet<ResourceKey>>,
    inner: R,
    key: ResourceKey,
}

impl<R: fmt::Debug> fmt::Debug for CachedResource<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<R> CachedResource<R> {
    /// Unpin this resource, making it eligible for LRU eviction.
    pub fn release(self) -> Self {
        self.set_released();
        self
    }

    /// Pin this resource in the LRU cache. It will not be evicted
    /// until [`release`](Self::release) is called for the same key.
    pub fn retain(self) -> Self {
        self.set_retained();
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.pinned.insert(self.key.clone());
    }

    /// Unpin this resource (by-ref, for use inside wrappers).
    pub(crate) fn set_released(&self) {
        self.pinned.remove(&self.key);
    }
}

impl<R: ResourceHandle + Clone + Send + Sync + fmt::Debug + 'static> ResourceHandle
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
    Resource {
        key: ResourceKey,
        identity: Option<RequestIdentity>,
        ctx: Option<C>,
    },
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
/// See crate `README.md` for the cache contract. Cache key is
/// `(ResourceKey, Option<RequestIdentity>, Option<Ctx>)`; the
/// `ResourceKey` carries its own asset namespace. Absolute keys bypass
/// caching (capability gate or absolute-key bypass).
#[derive(Clone)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    pinned: Arc<DashSet<ResourceKey>>,
    capacity: NonZeroUsize,
    on_invalidated: Option<crate::store::OnInvalidatedFn>,
    /// True when dropping a resource handle frees its bytes (ephemeral
    /// backing). Only then does LRU displacement mean the data is gone
    /// and `on_invalidated` must fire. For durable backends the bytes
    /// survive on disk, so displacement is a transparent cache miss.
    volatile: bool,
    cache: SharedCache<A>,
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
        volatile: bool,
    ) -> Self {
        Self {
            inner,
            capacity,
            on_invalidated,
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            pinned: Arc::new(DashSet::new()),
            volatile,
        }
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
            if self.volatile
                && let (
                    CacheKey::Resource {
                        key: resource_key, ..
                    },
                    CacheEntry::Resource(_),
                ) = (displaced_key, displaced_entry)
            {
                invalidated.push(resource_key);
            }
        }

        if cache.len() >= cache.cap().get() {
            let grow = NonZeroUsize::new(cache.len() + 1)
                .expect("BUG: cache overflow capacity must stay non-zero");
            cache.resize(grow);
        }

        if let Some((displaced_key, displaced_entry)) = cache.push(key, entry)
            && self.volatile
            && let (
                CacheKey::Resource {
                    key: resource_key, ..
                },
                CacheEntry::Resource(_),
            ) = (displaced_key, displaced_entry)
        {
            invalidated.push(resource_key);
        }

        invalidated
    }

    fn cached_state(&self, key: &ResourceKey) -> Option<AssetResourceState> {
        let (committed, has_active) = {
            let mut cache = self.cache.lock_sync();
            let mut committed = None;
            let mut has_active = false;
            let mut promote_key = None;

            for (cache_key, entry) in cache.iter() {
                let (
                    CacheKey::Resource {
                        key: resource_key, ..
                    },
                    CacheEntry::Resource(res),
                ) = (cache_key, entry)
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
                    ResourceStatus::Active | ResourceStatus::Cancelled => has_active = true,
                    ResourceStatus::Committed { final_len } => {
                        if committed.is_none() {
                            committed = Some(AssetResourceState::Committed { final_len });
                            promote_key = Some(cache_key.clone());
                        }
                    }
                }
            }

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

    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }

    fn is_active(&self) -> bool {
        self.inner.capabilities().contains(Capabilities::CACHE)
    }

    fn is_pinned_key(&self, key: &CacheKey<A::Context>) -> bool {
        match key {
            CacheKey::Resource {
                key: resource_key, ..
            } => self.pinned.contains(resource_key),
            _ => false,
        }
    }

    fn is_protected_resource(entry: &CacheEntry<A::Res, A::IndexRes>) -> bool {
        matches!(
            entry,
            CacheEntry::Resource(res) if matches!(res.status(), ResourceStatus::Active)
        )
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

    fn pinned_cache_count(&self, cache: &CacheMap<A>) -> usize {
        cache
            .iter()
            .filter(|(k, e)| {
                Self::is_protected_resource(e)
                    || matches!(k, CacheKey::Resource { key: rk, .. } if self.pinned.contains(rk))
            })
            .count()
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

    fn wrap(&self, key: &ResourceKey, inner: A::Res) -> CachedResource<A::Res> {
        CachedResource {
            inner,
            key: key.clone(),
            pinned: Arc::clone(&self.pinned),
        }
    }
}

impl<A> Assets for CachedAssets<A>
where
    A: Assets,
{
    type Context = A::Context;
    type IndexRes = A::IndexRes;
    type Res = CachedResource<A::Res>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if !self.is_active() || key.is_absolute() {
            return Ok(self.wrap(
                key,
                self.inner.acquire_resource_with_ctx(key, identity, ctx)?,
            ));
        }

        let cache_key = CacheKey::Resource {
            key: key.clone(),
            identity: identity.cloned(),
            ctx: ctx.clone(),
        };
        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            let res = res.clone();
            if !matches!(res.status(), ResourceStatus::Committed { .. }) {
                res.reactivate()?;
            }
            drop(cache);
            return Ok(self.wrap(key, res));
        }

        let res = self.inner.acquire_resource_with_ctx(key, identity, ctx)?;
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

    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        {
            let mut cache = self.cache.lock_sync();

            let keys_to_remove: Vec<CacheKey<A::Context>> = cache
                .iter()
                .filter_map(|(k, _)| match k {
                    CacheKey::Resource { key: rk, .. } if rk.asset_root() == Some(asset_root) => {
                        Some(k.clone())
                    }
                    _ => None,
                })
                .collect();

            for key in keys_to_remove {
                cache.pop(&key);
            }
        }

        self.inner.delete_asset(asset_root)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.open_index_resource(CacheKey::LruIndex, || self.inner.open_lru_index_resource())
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.open_index_resource(CacheKey::PinsIndex, || {
            self.inner.open_pins_index_resource()
        })
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        if !self.is_active() || key.is_absolute() {
            return Ok(self.wrap(key, self.inner.open_resource_with_ctx(key, identity, ctx)?));
        }

        let cache_key = CacheKey::Resource {
            key: key.clone(),
            identity: identity.cloned(),
            ctx: ctx.clone(),
        };

        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            return Ok(self.wrap(key, res.clone()));
        }

        if ctx.is_none()
            && let Some(res) =
                cache
                    .iter()
                    .find_map(|(candidate_key, entry)| match (candidate_key, entry) {
                        (
                            CacheKey::Resource {
                                key: resource_key, ..
                            },
                            CacheEntry::Resource(res),
                        ) if resource_key == key
                            && matches!(res.status(), ResourceStatus::Committed { .. }) =>
                        {
                            Some(res.clone())
                        }
                        _ => None,
                    })
        {
            return Ok(self.wrap(key, res));
        }

        let res = self.inner.open_resource_with_ctx(key, identity, ctx)?;
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

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        {
            let mut cache = self.cache.lock_sync();
            let keys_to_remove: Vec<_> = cache
                .iter()
                .filter_map(|(cache_key, _)| match cache_key {
                    CacheKey::Resource {
                        key: resource_key, ..
                    } if resource_key == key => Some(cache_key.clone()),
                    _ => None,
                })
                .collect();
            for cache_key in keys_to_remove {
                cache.pop(&cache_key);
            }
        }
        self.pinned.remove(key);
        self.inner.remove_resource(key)
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

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> Capabilities;
            fn root_dir(&self) -> &Path;
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{fs, path::Path, sync::Arc, time::Duration};

    use kithara_platform::thread;
    use kithara_storage::StorageResource;
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{base::Capabilities, disk_store::DiskAssetStore, mem_store::MemAssetStore};

    const ROOT: &str = "test_asset";

    #[derive(Clone, Debug)]
    struct ContextMemStore {
        inner: MemAssetStore,
    }

    impl Default for ContextMemStore {
        fn default() -> Self {
            Self {
                inner: MemAssetStore::new(
                    CancellationToken::new(),
                    None,
                    &crate::BytePool::default(),
                ),
            }
        }
    }

    impl Assets for ContextMemStore {
        type Context = u8;
        type IndexRes = StorageResource;
        type Res = StorageResource;

        fn acquire_resource_with_ctx(
            &self,
            key: &ResourceKey,
            identity: Option<&RequestIdentity>,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<Self::Res> {
            self.inner.acquire_resource(key, identity)
        }

        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }

        fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
            self.inner.delete_asset(asset_root)
        }

        fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
            self.inner.open_lru_index_resource()
        }

        fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
            self.inner.open_pins_index_resource()
        }

        fn open_resource_with_ctx(
            &self,
            key: &ResourceKey,
            identity: Option<&RequestIdentity>,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<Self::Res> {
            self.inner.open_resource(key, identity)
        }

        fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
            self.inner.resource_state(key)
        }

        fn root_dir(&self) -> &Path {
            self.inner.root_dir()
        }
    }

    fn make_cached(dir: &Path, capacity: NonZeroUsize) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(
            dir,
            CancellationToken::new(),
            &crate::BytePool::default(),
        ));
        CachedAssets::new(disk, capacity, None, false)
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn evicts_at_custom_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            let res = cached.acquire_resource(key, None).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        assert_eq!(cached.cache.lock_sync().len(), 3);
    }

    fn record_invalidations() -> (
        Arc<std::sync::Mutex<Vec<ResourceKey>>>,
        crate::store::OnInvalidatedFn,
    ) {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log_cb = Arc::clone(&log);
        let cb: crate::store::OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            log_cb
                .lock()
                .expect("invalidation log lock")
                .push(key.clone());
        });
        (log, cb)
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn durable_displacement_does_not_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let disk = Arc::new(DiskAssetStore::new(
            dir.path(),
            CancellationToken::new(),
            &crate::BytePool::default(),
        ));
        let (log, cb) = record_invalidations();
        let cached = CachedAssets::new(disk, NonZeroUsize::new(2).unwrap(), Some(cb), false);

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            let res = cached.acquire_resource(key, None).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // The oldest handle was displaced from the 2-slot in-memory LRU,
        // but its bytes survive on disk, so no invalidation must fire.
        assert_eq!(cached.cache.lock_sync().len(), 2);
        assert!(
            log.lock().expect("log lock").is_empty(),
            "durable backend must treat LRU displacement as transparent, got {:?}",
            log.lock().expect("log lock")
        );
        assert!(
            matches!(
                cached.resource_state(&keys[0]).unwrap(),
                AssetResourceState::Committed { .. }
            ),
            "displaced resource must remain committed on disk"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn volatile_displacement_invalidates() {
        let mem = Arc::new(MemAssetStore::new(
            CancellationToken::new(),
            None,
            &crate::BytePool::default(),
        ));
        let (log, cb) = record_invalidations();
        let cached = CachedAssets::new(mem, NonZeroUsize::new(2).unwrap(), Some(cb), true);

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            let res = cached.acquire_resource(key, None).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        // Ephemeral backing frees bytes when the last handle drops, so
        // displacement is real data loss and must invalidate the key.
        assert_eq!(cached.cache.lock_sync().len(), 2);
        assert_eq!(
            log.lock().expect("log lock").as_slice(),
            &[keys[0].clone()],
            "ephemeral backend must invalidate the displaced key"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn pinned_resources_survive_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..5)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();

        let first = cached.acquire_resource(&keys[0], None).unwrap().retain();
        first.write_at(0, b"data").unwrap();
        first.commit(Some(4)).unwrap();

        for key in &keys[1..] {
            let res = cached.acquire_resource(key, None).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }

        assert_eq!(cached.cache.lock_sync().len(), 4);
        assert!(
            matches!(
                cached.resource_state(&keys[0]),
                Ok(AssetResourceState::Committed { .. })
            ),
            "pinned resource must survive"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn release_unpins_and_allows_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..5)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();

        let first = cached.acquire_resource(&keys[0], None).unwrap().retain();
        first.write_at(0, b"data").unwrap();
        first.commit(Some(4)).unwrap();

        for key in &keys[1..4] {
            let res = cached.acquire_resource(key, None).unwrap();
            res.write_at(0, b"data").unwrap();
            res.commit(Some(4)).unwrap();
        }
        assert_eq!(cached.cache.lock_sync().len(), 4, "3 normal + 1 pinned");

        first.release();

        let res = cached.acquire_resource(&keys[4], None).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

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
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let res1 = cached.acquire_resource(&key, None).unwrap();
        let res2 = cached.acquire_resource(&key, None).unwrap();

        assert_eq!(res1.path(), res2.path());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn concurrent_opens_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        let res = cached.acquire_resource(&key, None).unwrap();
        res.write_at(0, b"hello").unwrap();
        res.commit(Some(5)).unwrap();

        let read_res = cached.open_resource(&key, None).unwrap();
        let mut buf = [0u8; 5];
        read_res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn delete_asset_clears_resource_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);

        let key = ResourceKey::relative(ROOT, "delete_me.mp3");
        let _res = cached.acquire_resource(&key, None).unwrap();

        assert!(!cached.cache.lock_sync().is_empty());

        cached.delete_asset(ROOT).unwrap();

        assert_eq!(cached.cache.lock_sync().len(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn remove_resource_clears_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);

        let key = ResourceKey::relative(ROOT, "remove_me.mp3");
        let res = cached.acquire_resource(&key, None).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        assert!(matches!(
            cached.resource_state(&key),
            Ok(AssetResourceState::Committed { .. })
        ));

        cached.remove_resource(&key).unwrap();

        let state = cached.resource_state(&key).unwrap();
        assert_eq!(state, AssetResourceState::Missing);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_returns_committed_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::relative(ROOT, "committed.m4s");

        let res = cached.acquire_resource(&key, None).unwrap();
        res.write_at(0, b"hello").unwrap();
        res.commit(Some(5)).unwrap();

        let opened = cached.open_resource(&key, None).unwrap();
        let mut buf = [0u8; 5];
        opened.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn context_aware_caching_separates_keys() {
        let store = ContextMemStore::default();
        let cached = CachedAssets::new(Arc::new(store), NonZeroUsize::new(5).unwrap(), None, true);
        let key = ResourceKey::relative("ctx_test", "seg.m4s");

        let r1 = cached
            .acquire_resource_with_ctx(&key, None, Some(1))
            .unwrap();
        r1.write_at(0, b"aaa").unwrap();
        r1.commit(Some(3)).unwrap();

        let r2 = cached
            .acquire_resource_with_ctx(&key, None, Some(2))
            .unwrap();
        r2.write_at(0, b"bbb").unwrap();
        r2.commit(Some(3)).unwrap();

        let check1 = cached.open_resource_with_ctx(&key, None, Some(1)).unwrap();
        let check2 = cached.open_resource_with_ctx(&key, None, Some(2)).unwrap();
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

        let key_a = ResourceKey::relative(ROOT, "a.m4s");
        let a = cached.acquire_resource(&key_a, None).unwrap();
        a.write_at(0, b"aaaa").unwrap();
        a.commit(Some(4)).unwrap();

        let key_b = ResourceKey::relative(ROOT, "b.m4s");
        let b = cached.acquire_resource(&key_b, None).unwrap();
        b.write_at(0, b"bbbb").unwrap();
        b.commit(Some(4)).unwrap();

        let key_c = ResourceKey::relative(ROOT, "c.m4s");
        let c = cached.acquire_resource(&key_c, None).unwrap();
        c.write_at(0, b"cccc").unwrap();
        c.commit(Some(4)).unwrap();

        let a_path = dir.path().join(ROOT).join("a.m4s");
        assert!(a_path.exists(), "committed data must remain on disk");
        assert_eq!(fs::read(&a_path).unwrap(), b"aaaa");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_with_none_context_finds_committed() {
        let store = ContextMemStore::default();
        let cached = CachedAssets::new(Arc::new(store), NonZeroUsize::new(5).unwrap(), None, true);
        let key = ResourceKey::relative("ctx_test2", "seg.m4s");

        let res = cached
            .acquire_resource_with_ctx(&key, None, Some(42))
            .unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        let found = cached.open_resource(&key, None).unwrap();
        let mut buf = [0u8; 4];
        found.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"data");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn concurrent_acquire_returns_same_resource() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = Arc::new(make_cached(dir.path(), cap));

        let key = ResourceKey::relative(ROOT, "concurrent.m4s");
        let key2 = key.clone();
        let cached2 = Arc::clone(&cached);

        let h = thread::spawn(move || {
            let _res = cached2.acquire_resource(&key2, None).unwrap();
        });

        let _res = cached.acquire_resource(&key, None).unwrap();
        h.join().unwrap();

        assert_eq!(cached.cache.lock_sync().len(), 1);
    }
}
