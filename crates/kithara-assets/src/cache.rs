#![forbid(unsafe_code)]

use std::{fmt, num::NonZeroUsize, ops::Range, path::Path, sync::Arc};

use dashmap::DashSet;
use kithara_platform::Mutex;
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};
use lru::LruCache;

use crate::{
    AssetResourceState,
    acquisition::{AcquisitionResult, RawWriteHandle, ReadSide, WriteSide},
    base::{Assets, Capabilities},
    error::AssetsResult,
    identity::RequestIdentity,
    key::ResourceKey,
};

/// Writer (Pending) wrapper returned by [`CachedAssets`].
pub struct CachedWriter<W> {
    pinned: Arc<DashSet<ResourceKey>>,
    inner: W,
    key: ResourceKey,
}

/// Reader (Ready) wrapper returned by [`CachedAssets`]. Cheap to clone.
#[derive(Clone)]
pub struct CachedReader<R> {
    pinned: Arc<DashSet<ResourceKey>>,
    inner: R,
    key: ResourceKey,
}

impl<W: fmt::Debug> fmt::Debug for CachedWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<R: fmt::Debug> fmt::Debug for CachedReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<W> CachedWriter<W> {
    /// Pin this resource in the LRU cache so it is never evicted, until
    /// [`CachedReader::release`] is called for the same key.
    pub fn retain(self) -> Self {
        self.pinned.insert(self.key.clone());
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.pinned.insert(self.key.clone());
    }
}

impl<R> CachedReader<R> {
    /// Unpin this resource, making it eligible for LRU eviction.
    pub fn release(self) -> Self {
        self.pinned.remove(&self.key);
        self
    }

    /// Pin this resource in the LRU cache. It will not be evicted
    /// until [`release`](Self::release) is called for the same key.
    pub fn retain(self) -> Self {
        self.pinned.insert(self.key.clone());
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.pinned.insert(self.key.clone());
    }
}

impl<W: WriteSide> WriteSide for CachedWriter<W> {
    type Reader = CachedReader<W::Reader>;

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn reader(&self) -> CachedReader<W::Reader> {
        CachedReader {
            pinned: Arc::clone(&self.pinned),
            inner: self.inner.reader(),
            key: self.key.clone(),
        }
    }

    fn raw_write_handle(&self) -> RawWriteHandle {
        self.inner.raw_write_handle()
    }

    fn commit(self, final_len: Option<u64>) -> StorageResult<CachedReader<W::Reader>> {
        Ok(CachedReader {
            pinned: Arc::clone(&self.pinned),
            inner: self.inner.commit(final_len)?,
            key: self.key.clone(),
        })
    }

    fn fail(self, reason: String) {
        self.inner.fail(reason);
    }
}

impl<R: ReadSide> ReadSide for CachedReader<R> {
    type Writer = CachedWriter<R::Writer>;

    fn reactivate(self) -> StorageResult<CachedWriter<R::Writer>> {
        Ok(CachedWriter {
            pinned: Arc::clone(&self.pinned),
            inner: self.inner.reactivate()?,
            key: self.key.clone(),
        })
    }

    delegate::delegate! {
        to self.inner {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
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
    Arc<Cache<<A as Assets>::ReadyRes, <A as Assets>::Context, <A as Assets>::IndexRes>>;
type CacheMap<A> = LruCache<
    CacheKey<<A as Assets>::Context>,
    CacheEntry<<A as Assets>::ReadyRes, <A as Assets>::IndexRes>,
>;
type CacheItem<A> = (
    CacheKey<<A as Assets>::Context>,
    CacheEntry<<A as Assets>::ReadyRes, <A as Assets>::IndexRes>,
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
        entry: CacheEntry<A::ReadyRes, A::IndexRes>,
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

    fn is_protected_resource(entry: &CacheEntry<A::ReadyRes, A::IndexRes>) -> bool {
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

    fn wrap_writer(&self, key: &ResourceKey, inner: A::ActiveRes) -> CachedWriter<A::ActiveRes> {
        CachedWriter {
            inner,
            key: key.clone(),
            pinned: Arc::clone(&self.pinned),
        }
    }

    fn wrap_reader(&self, key: &ResourceKey, inner: A::ReadyRes) -> CachedReader<A::ReadyRes> {
        CachedReader {
            inner,
            key: key.clone(),
            pinned: Arc::clone(&self.pinned),
        }
    }

    /// Wrap an [`AcquisitionResult`] from the inner store in cache wrappers
    /// (used on the bypass and absolute-key paths where no caching happens).
    fn wrap_acq(
        &self,
        key: &ResourceKey,
        acq: AcquisitionResult<A::ActiveRes, A::ReadyRes>,
    ) -> AcquisitionResult<CachedWriter<A::ActiveRes>, CachedReader<A::ReadyRes>> {
        match acq {
            AcquisitionResult::Pending(w) => AcquisitionResult::Pending(self.wrap_writer(key, w)),
            AcquisitionResult::Ready(r) => AcquisitionResult::Ready(self.wrap_reader(key, r)),
        }
    }
}

impl<A> Assets for CachedAssets<A>
where
    A: Assets,
{
    type ActiveRes = CachedWriter<A::ActiveRes>;
    type Context = A::Context;
    type IndexRes = A::IndexRes;
    type ReadyRes = CachedReader<A::ReadyRes>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
        if !self.is_active() || key.is_absolute() {
            return Ok(self.wrap_acq(
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

        let hit = match cache.get(&cache_key) {
            Some(CacheEntry::Resource(reader)) => Some(reader.clone()),
            _ => None,
        };
        if let Some(reader) = hit {
            if matches!(reader.status(), ResourceStatus::Committed { .. }) {
                drop(cache);
                return Ok(AcquisitionResult::Ready(self.wrap_reader(key, reader)));
            }
            // In-flight slot: reactivate mints a fresh-generation writer; cache
            // its current-generation reader-view so concurrent opens block on
            // the new generation's gate. No invalidation — same key, same bytes.
            let writer = reader.reactivate()?;
            cache.put(cache_key, CacheEntry::Resource(writer.reader()));
            drop(cache);
            return Ok(AcquisitionResult::Pending(self.wrap_writer(key, writer)));
        }

        let acq = self.inner.acquire_resource_with_ctx(key, identity, ctx)?;
        let reader_for_cache = match &acq {
            AcquisitionResult::Pending(w) => w.reader(),
            AcquisitionResult::Ready(r) => r.clone(),
        };
        let displaced = self.cache_entry(
            &mut cache,
            cache_key,
            CacheEntry::Resource(reader_for_cache),
        );
        let on_invalidated = self.on_invalidated.clone();
        drop(cache);

        if let Some(cb) = on_invalidated {
            for displaced_key in displaced {
                cb(&displaced_key);
            }
        }

        Ok(self.wrap_acq(key, acq))
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
    ) -> AssetsResult<Self::ReadyRes> {
        if !self.is_active() || key.is_absolute() {
            return Ok(
                self.wrap_reader(key, self.inner.open_resource_with_ctx(key, identity, ctx)?)
            );
        }

        let cache_key = CacheKey::Resource {
            key: key.clone(),
            identity: identity.cloned(),
            ctx: ctx.clone(),
        };

        let mut cache = self.cache.lock_sync();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            return Ok(self.wrap_reader(key, res.clone()));
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
            return Ok(self.wrap_reader(key, res));
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

        Ok(self.wrap_reader(key, res))
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
    use std::{fs, path::Path, sync::Arc};

    use kithara_platform::{CancelToken, thread, time::Duration};
    use kithara_storage::StorageResource;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        base::{BaseReader, BaseWriter, Capabilities},
        disk_store::DiskAssetStore,
        mem_store::MemAssetStore,
    };

    const ROOT: &str = "test_asset";

    /// Stream `data` through a Pending writer and commit it.
    fn commit_writer<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>, data: &[u8]) {
        let AcquisitionResult::Pending(w) = acq else {
            panic!("expected a Pending writer");
        };
        w.write_at(0, data).unwrap();
        w.commit(Some(data.len() as u64)).unwrap();
    }

    /// Extract the Pending writer or panic.
    fn pending<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>) -> W {
        match acq {
            AcquisitionResult::Pending(w) => w,
            AcquisitionResult::Ready(_) => panic!("expected a Pending writer"),
        }
    }

    #[derive(Clone, Debug)]
    struct ContextMemStore {
        inner: MemAssetStore,
    }

    impl Default for ContextMemStore {
        fn default() -> Self {
            Self {
                inner: MemAssetStore::new(CancelToken::never(), None, &crate::BytePool::default()),
            }
        }
    }

    impl Assets for ContextMemStore {
        type ActiveRes = BaseWriter;
        type Context = u8;
        type IndexRes = StorageResource;
        type ReadyRes = BaseReader;

        fn acquire_resource_with_ctx(
            &self,
            key: &ResourceKey,
            identity: Option<&RequestIdentity>,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<AcquisitionResult<BaseWriter, BaseReader>> {
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
        ) -> AssetsResult<BaseReader> {
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
            CancelToken::never(),
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
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
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
            CancelToken::never(),
            &crate::BytePool::default(),
        ));
        let (log, cb) = record_invalidations();
        let cached = CachedAssets::new(disk, NonZeroUsize::new(2).unwrap(), Some(cb), false);

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
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
            CancelToken::never(),
            None,
            &crate::BytePool::default(),
        ));
        let (log, cb) = record_invalidations();
        let cached = CachedAssets::new(mem, NonZeroUsize::new(2).unwrap(), Some(cb), true);

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
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

        let first = pending(cached.acquire_resource(&keys[0], None).unwrap()).retain();
        first.write_at(0, b"data").unwrap();
        first.commit(Some(4)).unwrap();

        for key in &keys[1..] {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
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

        let first = pending(cached.acquire_resource(&keys[0], None).unwrap()).retain();
        first.write_at(0, b"data").unwrap();
        let first = first.commit(Some(4)).unwrap();

        for key in &keys[1..4] {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
        }
        assert_eq!(cached.cache.lock_sync().len(), 4, "3 normal + 1 pinned");

        first.release();

        commit_writer(cached.acquire_resource(&keys[4], None).unwrap(), b"data");

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

        let res1 = pending(cached.acquire_resource(&key, None).unwrap());
        let res2 = pending(cached.acquire_resource(&key, None).unwrap());

        assert_eq!(res1.reader().path(), res2.reader().path());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn concurrent_opens_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::relative(ROOT, "audio.mp3");

        commit_writer(cached.acquire_resource(&key, None).unwrap(), b"hello");

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
        let _res = pending(cached.acquire_resource(&key, None).unwrap());

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
        commit_writer(cached.acquire_resource(&key, None).unwrap(), b"data");

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

        commit_writer(cached.acquire_resource(&key, None).unwrap(), b"hello");

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

        commit_writer(
            cached
                .acquire_resource_with_ctx(&key, None, Some(1))
                .unwrap(),
            b"aaa",
        );
        commit_writer(
            cached
                .acquire_resource_with_ctx(&key, None, Some(2))
                .unwrap(),
            b"bbb",
        );

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
        commit_writer(cached.acquire_resource(&key_a, None).unwrap(), b"aaaa");

        let key_b = ResourceKey::relative(ROOT, "b.m4s");
        commit_writer(cached.acquire_resource(&key_b, None).unwrap(), b"bbbb");

        let key_c = ResourceKey::relative(ROOT, "c.m4s");
        commit_writer(cached.acquire_resource(&key_c, None).unwrap(), b"cccc");

        let a_path = dir.path().join(ROOT).join("a.m4s");
        assert!(a_path.exists(), "committed data must remain on disk");
        assert_eq!(fs::read(&a_path).unwrap(), b"aaaa");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn open_resource_with_none_context_finds_committed() {
        let store = ContextMemStore::default();
        let cached = CachedAssets::new(Arc::new(store), NonZeroUsize::new(5).unwrap(), None, true);
        let key = ResourceKey::relative("ctx_test2", "seg.m4s");

        commit_writer(
            cached
                .acquire_resource_with_ctx(&key, None, Some(42))
                .unwrap(),
            b"data",
        );

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
