#![forbid(unsafe_code)]

use std::{fmt, num::NonZeroUsize, path::Path};

use dashmap::DashSet;
use kithara_platform::sync::{Arc, Mutex, Weak};
use kithara_storage::ResourceStatus;
use lru::LruCache;

use super::{
    handle::{CachedReader, CachedWriter, EnforceCapacity},
    policy::CachePolicy,
};
use crate::{
    decorator::{Assets, Capabilities},
    error::AssetsResult,
    layout::ResourceKey,
    resource::{AcquisitionResult, AssetResourceState, ReadSide, RequestIdentity, WriteSide},
};

/// Opaque byte discriminator for cache entries. `Debug` is redacted: the
/// bytes can be key material (e.g. AES `key||iv`).
#[derive(Clone, derive_more::Debug, Eq, PartialEq, Hash)]
#[debug("CtxIdentity(<{} bytes>)", _0.len())]
pub(crate) struct CtxIdentity(Box<[u8]>);

impl From<&[u8]> for CtxIdentity {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.into())
    }
}

/// Projects a processing context to its [`CtxIdentity`] cache discriminator.
pub(crate) trait CacheIdentity {
    fn cache_identity(&self) -> CtxIdentity;
}

impl CacheIdentity for crate::decorator::ProcessCtx {
    fn cache_identity(&self) -> CtxIdentity {
        self.identity().into()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct CacheKey {
    key: ResourceKey,
    identity: Option<RequestIdentity>,
    ctx: Option<CtxIdentity>,
}

#[derive(Clone, Debug)]
struct CacheEntry<R> {
    reader: R,
    hits: usize,
    writer: Weak<()>,
}

impl<R: ReadSide> CacheEntry<R> {
    fn is_committed(&self) -> bool {
        !self.is_writing() && matches!(self.reader.status(), ResourceStatus::Committed { .. })
    }

    fn is_dead(&self) -> bool {
        !self.is_writing() && !self.is_committed()
    }

    fn is_writing(&self) -> bool {
        self.writer.strong_count() > 0
    }
}

type Cache<R> = Mutex<LruCache<CacheKey, CacheEntry<R>>>;
type SharedCache<A> = Arc<Cache<<A as Assets>::ReadyRes>>;
type CacheMap<A> = LruCache<CacheKey, CacheEntry<<A as Assets>::ReadyRes>>;

/// A decorator that caches opened resources in memory with LRU eviction.
///
/// Cache key is `(ResourceKey, Option<RequestIdentity>, Option<Ctx>)`; the
/// `ResourceKey` carries its own asset namespace. Absolute keys bypass caching.
#[derive_where::derive_where(Clone; A: Assets, C: Clone)]
pub struct CachedAssets<A, C = (NonZeroUsize, Option<u64>)>
where
    A: Assets,
    C: CachePolicy,
{
    inner: Arc<A>,
    pinned: Arc<DashSet<ResourceKey>>,
    policy: C,
    enforce_capacity: Option<EnforceCapacity>,
    on_invalidated: Option<crate::store::OnInvalidatedFn>,
    cache: SharedCache<A>,
    /// True when dropping a resource handle frees its bytes (ephemeral
    /// backing). Only then does LRU displacement mean the data is gone
    /// and `on_invalidated` must fire. For durable backends the bytes
    /// survive on disk, so displacement is a transparent cache miss.
    volatile: bool,
}

impl<A, C> fmt::Debug for CachedAssets<A, C>
where
    A: Assets,
    C: CachePolicy,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = self.cache.try_lock().ok().map(|c| c.len());
        f.debug_struct("CachedAssets")
            .field("cache_size", &size)
            .finish_non_exhaustive()
    }
}

impl<A, C> CachedAssets<A, C>
where
    A: Assets,
    C: CachePolicy,
{
    pub(crate) fn cache_capacity(&self) -> NonZeroUsize {
        self.policy.capacity()
    }

    fn cache_entry(
        &self,
        cache: &mut CacheMap<A>,
        key: CacheKey,
        entry: CacheEntry<A::ReadyRes>,
    ) -> Vec<ResourceKey> {
        let mut invalidated: Vec<ResourceKey> = Vec::new();

        let effective = self.policy.capacity().get() + self.pinned_cache_count(cache);

        while cache.len() >= effective {
            let Some(displaced) = self.pop_evictable(cache) else {
                break;
            };
            if self.volatile {
                invalidated.push(displaced.key);
            }
        }

        if cache.len() >= cache.cap().get() {
            let grow = NonZeroUsize::new(cache.len() + 1)
                .expect("BUG: cache overflow capacity must stay non-zero");
            cache.resize(grow);
        }

        if let Some((displaced, _)) = cache.push(key, entry)
            && self.volatile
        {
            invalidated.push(displaced.key);
        }

        invalidated
    }

    fn cached_state(&self, key: &ResourceKey) -> Option<AssetResourceState> {
        let (committed, has_active) = {
            let mut cache = self.cache.lock();
            let mut committed = None;
            let mut has_active = false;
            let mut promote_key = None;

            for (cache_key, entry) in cache.iter() {
                if cache_key.key != *key {
                    continue;
                }

                match entry.reader.status() {
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

    /// A committed resource of unknown length counts as unbounded, so it cannot stay in a
    /// byte-bounded cache.
    fn committed_bytes(entry: &CacheEntry<A::ReadyRes>) -> Option<u64> {
        let ResourceStatus::Committed { final_len } = entry.reader.status() else {
            return None;
        };
        Some(final_len.or_else(|| entry.reader.len()).unwrap_or(u64::MAX))
    }

    fn is_pinned(pinned: &DashSet<ResourceKey>, key: &CacheKey) -> bool {
        pinned.contains(&key.key)
    }

    fn is_pinned_key(&self, key: &CacheKey) -> bool {
        Self::is_pinned(&self.pinned, key)
    }

    fn is_protected_resource(entry: &CacheEntry<A::ReadyRes>) -> bool {
        matches!(entry.reader.status(), ResourceStatus::Active)
    }

    fn pinned_cache_count(&self, cache: &CacheMap<A>) -> usize {
        cache
            .iter()
            .filter(|(k, e)| Self::is_protected_resource(e) || self.pinned.contains(&k.key))
            .count()
    }

    /// The victim is chosen by fewest hits, with ties falling to the least-recently-used end, since
    /// iteration yields most- to least-recently-used and a later equal candidate is the better
    /// victim.
    fn pop_evictable(&self, cache: &mut CacheMap<A>) -> Option<CacheKey> {
        let key = cache
            .iter()
            .filter_map(|(key, entry)| {
                if Self::is_protected_resource(entry) || self.is_pinned_key(key) {
                    return None;
                }
                Some((key.clone(), entry.hits))
            })
            .reduce(|best, cand| if cand.1 <= best.1 { cand } else { best })
            .map(|(key, _)| key)?;
        cache.pop(&key)?;
        Some(key)
    }

    fn retained_bytes(cache: &CacheMap<A>, pinned: &DashSet<ResourceKey>) -> u64 {
        cache
            .iter()
            .filter(|(key, _)| !Self::is_pinned(pinned, key))
            .filter_map(|(_, entry)| Self::committed_bytes(entry))
            .fold(0_u64, u64::saturating_add)
    }

    fn trim_to_max_bytes(
        cache: &mut CacheMap<A>,
        pinned: &DashSet<ResourceKey>,
        max_bytes: u64,
    ) -> Vec<ResourceKey> {
        let mut invalidated: Vec<ResourceKey> = Vec::new();
        while Self::retained_bytes(cache, pinned) > max_bytes {
            let Some(key) = cache
                .iter()
                .filter_map(|(key, entry)| {
                    let bytes = Self::committed_bytes(entry)?;
                    if bytes == 0 || Self::is_pinned(pinned, key) {
                        return None;
                    }
                    Some((key.clone(), entry.hits))
                })
                .reduce(|best, candidate| {
                    if candidate.1 <= best.1 {
                        candidate
                    } else {
                        best
                    }
                })
                .map(|(key, _)| key)
            else {
                break;
            };
            if cache.pop(&key).is_some() && !invalidated.contains(&key.key) {
                invalidated.push(key.key);
            }
        }
        invalidated
    }

    pub(crate) fn with_policy(
        inner: Arc<A>,
        policy: C,
        on_invalidated: Option<crate::store::OnInvalidatedFn>,
        volatile: bool,
    ) -> Self {
        let cache = Arc::new(Mutex::new(LruCache::new(policy.capacity())));
        let pinned = Arc::new(DashSet::new());
        let enforce_capacity = (volatile && policy.max_bytes().is_some()).then(|| {
            let cache = Arc::clone(&cache);
            let pinned = Arc::clone(&pinned);
            let on_invalidated = on_invalidated.clone();
            let policy = policy.clone();
            Arc::new(move || {
                let invalidated = {
                    let mut cache = cache.lock();
                    policy.max_bytes().map_or_else(Vec::new, |max_bytes| {
                        Self::trim_to_max_bytes(&mut cache, &pinned, max_bytes)
                    })
                };
                if let Some(cb) = &on_invalidated {
                    for key in invalidated {
                        cb(&key);
                    }
                }
            }) as EnforceCapacity
        });
        Self {
            inner,
            pinned,
            policy,
            enforce_capacity,
            on_invalidated,
            cache,
            volatile,
        }
    }

    /// Wrap an [`AcquisitionResult`] from the inner store in cache wrappers.
    fn wrap_acq(
        &self,
        key: &ResourceKey,
        acq: AcquisitionResult<A::ActiveRes, A::ReadyRes>,
        writing: Arc<()>,
    ) -> AcquisitionResult<CachedWriter<A::ActiveRes>, CachedReader<A::ReadyRes>> {
        match acq {
            AcquisitionResult::Pending(w) => {
                AcquisitionResult::Pending(self.wrap_writer(key, w, writing))
            }
            AcquisitionResult::Ready(r) => AcquisitionResult::Ready(self.wrap_reader(key, r)),
            AcquisitionResult::Joined(r) => AcquisitionResult::Joined(self.wrap_reader(key, r)),
        }
    }

    fn wrap_reader(&self, key: &ResourceKey, inner: A::ReadyRes) -> CachedReader<A::ReadyRes> {
        CachedReader::new(
            Arc::clone(&self.pinned),
            key.clone(),
            inner,
            self.enforce_capacity.clone(),
        )
    }

    fn wrap_writer(
        &self,
        key: &ResourceKey,
        inner: A::ActiveRes,
        writing: Arc<()>,
    ) -> CachedWriter<A::ActiveRes> {
        CachedWriter::new(
            Arc::clone(&self.pinned),
            key.clone(),
            inner,
            self.enforce_capacity.clone(),
            writing,
        )
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
        Self::with_max_bytes(inner, capacity, on_invalidated, volatile, None)
    }

    pub(crate) fn with_max_bytes(
        inner: Arc<A>,
        capacity: NonZeroUsize,
        on_invalidated: Option<crate::store::OnInvalidatedFn>,
        volatile: bool,
        max_bytes: Option<u64>,
    ) -> Self {
        Self::with_policy(inner, (capacity, max_bytes), on_invalidated, volatile)
    }
}

impl<A, C> Assets for CachedAssets<A, C>
where
    A: Assets,
    A::Context: CacheIdentity,
    C: CachePolicy,
{
    type ActiveRes = CachedWriter<A::ActiveRes>;
    type Context = A::Context;
    type ReadyRes = CachedReader<A::ReadyRes>;

    /// A live writer stays the only writer of its generation, so a second party joins as a reader.
    /// A slot without one mints a fresh-generation writer, and its current-generation reader view
    /// is cached so concurrent opens block on the new generation's gate.
    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
        if key.is_absolute() {
            return Ok(self.wrap_acq(
                key,
                self.inner.acquire_resource_with_ctx(key, identity, ctx)?,
                Arc::new(()),
            ));
        }

        let cache_key = CacheKey {
            key: key.clone(),
            identity: identity.cloned(),
            ctx: ctx.as_ref().map(CacheIdentity::cache_identity),
        };
        let mut cache = self.cache.lock();

        let hit = cache.get_mut(&cache_key).map(|entry| {
            entry.hits = entry.hits.saturating_add(1);
            entry.clone()
        });
        if let Some(hit) = hit {
            if hit.is_writing() {
                drop(cache);
                return Ok(AcquisitionResult::Joined(self.wrap_reader(key, hit.reader)));
            }
            if hit.is_committed() {
                drop(cache);
                return Ok(AcquisitionResult::Ready(self.wrap_reader(key, hit.reader)));
            }
            let writer = hit.reader.reactivate()?;
            let writing = Arc::new(());
            cache.put(
                cache_key,
                CacheEntry {
                    reader: writer.reader(),
                    hits: hit.hits,
                    writer: Arc::downgrade(&writing),
                },
            );
            drop(cache);
            return Ok(AcquisitionResult::Pending(
                self.wrap_writer(key, writer, writing),
            ));
        }

        let acq = self.inner.acquire_resource_with_ctx(key, identity, ctx)?;
        let writing = Arc::new(());
        let (reader_for_cache, writer) = match &acq {
            AcquisitionResult::Pending(w) => (w.reader(), Arc::downgrade(&writing)),
            AcquisitionResult::Ready(r) | AcquisitionResult::Joined(r) => (r.clone(), Weak::new()),
        };
        let displaced = self.cache_entry(
            &mut cache,
            cache_key,
            CacheEntry {
                reader: reader_for_cache,
                hits: 1,
                writer,
            },
        );
        let on_invalidated = self.on_invalidated.clone();
        drop(cache);

        if let Some(cb) = on_invalidated {
            for displaced_key in displaced {
                cb(&displaced_key);
            }
        }
        if let Some(enforce) = &self.enforce_capacity {
            enforce();
        }

        Ok(self.wrap_acq(key, acq, writing))
    }

    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()> {
        {
            let mut cache = self.cache.lock();

            let keys_to_remove: Vec<CacheKey> = cache
                .iter()
                .filter(|(k, _)| k.key.asset_root() == Some(asset_root))
                .map(|(k, _)| k.clone())
                .collect();

            for key in keys_to_remove {
                cache.pop(&key);
            }
        }

        self.inner.delete_asset(asset_root)
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::ReadyRes> {
        if key.is_absolute() {
            return Ok(
                self.wrap_reader(key, self.inner.open_resource_with_ctx(key, identity, ctx)?)
            );
        }

        let cache_key = CacheKey {
            key: key.clone(),
            identity: identity.cloned(),
            ctx: ctx.as_ref().map(CacheIdentity::cache_identity),
        };

        let mut cache = self.cache.lock();

        let dead = cache.peek(&cache_key).is_some_and(CacheEntry::is_dead);
        if !dead && let Some(entry) = cache.get_mut(&cache_key) {
            entry.hits = entry.hits.saturating_add(1);
            return Ok(self.wrap_reader(key, entry.reader.clone()));
        }

        if ctx.is_none()
            && let Some(res) = cache
                .iter()
                .find(|(candidate_key, entry)| candidate_key.key == *key && entry.is_committed())
                .map(|(_, entry)| entry.reader.clone())
        {
            return Ok(self.wrap_reader(key, res));
        }

        let res = self.inner.open_resource_with_ctx(key, identity, ctx)?;
        if dead {
            drop(cache);
            return Ok(self.wrap_reader(key, res));
        }
        let displaced = self.cache_entry(
            &mut cache,
            cache_key,
            CacheEntry {
                reader: res.clone(),
                hits: 1,
                writer: Weak::new(),
            },
        );
        let on_invalidated = self.on_invalidated.clone();
        drop(cache);

        if let Some(cb) = on_invalidated {
            for displaced_key in displaced {
                cb(&displaced_key);
            }
        }
        if let Some(enforce) = &self.enforce_capacity {
            enforce();
        }

        Ok(self.wrap_reader(key, res))
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        {
            let mut cache = self.cache.lock();
            let keys_to_remove: Vec<CacheKey> = cache
                .iter()
                .filter(|(cache_key, _)| cache_key.key == *key)
                .map(|(cache_key, _)| cache_key.clone())
                .collect();
            for cache_key in keys_to_remove {
                cache.pop(&cache_key);
            }
        }
        self.pinned.remove(key);
        self.inner.remove_resource(key)
    }

    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState> {
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
    use std::{fs, path::Path};

    use kithara_platform::{CancelToken, sync::Arc, thread, time::Duration, tokio::sync::mpsc};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        backend::{DiskAssetStore, MemAssetStore},
        consts,
        decorator::Capabilities,
        resource::{BaseReader, BaseWriter},
    };

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
            AcquisitionResult::Ready(_) | AcquisitionResult::Joined(_) => {
                panic!("expected a Pending writer")
            }
        }
    }

    /// The base stores (`DiskAssetStore` / `MemAssetStore`) carry `Context =
    /// ()`: no processing, so every entry shares one empty identity.
    impl CacheIdentity for () {
        fn cache_identity(&self) -> CtxIdentity {
            [].as_slice().into()
        }
    }

    /// Exact-byte cache identity for the `u8` mock context: the single byte
    /// is its own discriminator, so two distinct ctx values key separately.
    impl CacheIdentity for u8 {
        fn cache_identity(&self) -> CtxIdentity {
            [*self].as_slice().into()
        }
    }

    /// The identity bytes can be AES `key||iv`; `Debug` must never print them,
    /// neither directly nor via `CacheKey`'s derived `Debug`.
    #[kithara::test(native, flash(false))]
    fn debug_redacts_identity_bytes() {
        let key = [0xAB_u8; 16];
        let iv = [0xCD_u8; 16];
        let bytes: Vec<u8> = key.iter().chain(iv.iter()).copied().collect();
        let identity = CtxIdentity::from(bytes.as_slice());

        let direct = format!("{identity:?}");
        assert_eq!(direct, "CtxIdentity(<32 bytes>)");

        let cache_key = CacheKey {
            key: ResourceKey::relative("root", "seg.m4s"),
            identity: None,
            ctx: Some(identity),
        };
        let via_key = format!("{cache_key:?}");
        assert!(via_key.contains("CtxIdentity(<32 bytes>)"), "{via_key}");
        assert!(
            !via_key.contains("171"),
            "leaked key byte 0xAB (171): {via_key}"
        );
        assert!(
            !via_key.contains("205"),
            "leaked iv byte 0xCD (205): {via_key}"
        );
    }

    #[derive(Clone, Debug)]
    struct ContextMemStore {
        inner: MemAssetStore<crate::test_pools::TestPools>,
    }

    impl Default for ContextMemStore {
        fn default() -> Self {
            Self {
                inner: MemAssetStore::new(CancelToken::never(), None, crate::test_pools::pools()),
            }
        }
    }

    impl Assets for ContextMemStore {
        type ActiveRes = BaseWriter;
        type Context = u8;
        type ReadyRes = BaseReader;

        fn acquire_resource_with_ctx(
            &self,
            key: &ResourceKey,
            identity: Option<&RequestIdentity>,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<AcquisitionResult<BaseWriter, BaseReader>> {
            self.inner.acquire_resource(key, identity)
        }

        fn open_resource_with_ctx(
            &self,
            key: &ResourceKey,
            identity: Option<&RequestIdentity>,
            _ctx: Option<Self::Context>,
        ) -> AssetsResult<BaseReader> {
            self.inner.open_resource(key, identity)
        }

        delegate::delegate! {
            to self.inner {
                fn capabilities(&self) -> Capabilities;
                fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;
                fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;
                fn root_dir(&self) -> &Path;
            }
        }
    }

    fn make_cached(dir: &Path, capacity: NonZeroUsize) -> CachedAssets<DiskAssetStore> {
        let disk = Arc::new(DiskAssetStore::new(dir, CancelToken::never()));
        CachedAssets::new(disk, capacity, None, false)
    }

    #[derive(Clone)]
    struct MockCachePolicy(Arc<Mutex<NonZeroUsize>>);

    impl CachePolicy for MockCachePolicy {
        fn capacity(&self) -> NonZeroUsize {
            *self.0.lock()
        }

        fn max_bytes(&self) -> Option<u64> {
            None
        }
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn cache_reads_capacity_from_its_policy_on_each_insert() {
        let dir = tempfile::tempdir().unwrap();
        let capacity = Arc::new(Mutex::new(NonZeroUsize::new(3).unwrap()));
        let disk = Arc::new(DiskAssetStore::new(dir.path(), CancelToken::never()));
        let cached =
            CachedAssets::with_policy(disk, MockCachePolicy(Arc::clone(&capacity)), None, false);
        for i in 0..3 {
            let key = ResourceKey::relative(consts::LAYER_ROOT, format!("seg_{i}.m4s"));
            commit_writer(cached.acquire_resource(&key, None).unwrap(), b"data");
        }
        *capacity.lock() = NonZeroUsize::new(1).unwrap();
        let key = ResourceKey::relative(consts::LAYER_ROOT, "seg_3.m4s");
        commit_writer(cached.acquire_resource(&key, None).unwrap(), b"data");
        assert_eq!(cached.cache.lock().len(), 1);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn evicts_at_custom_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..4)
            .map(|i| ResourceKey::relative(consts::LAYER_ROOT, format!("seg_{i}.m4s")))
            .collect();

        for key in &keys {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
        }

        assert_eq!(cached.cache.lock().len(), 3);
    }

    fn record_invalidations() -> (
        mpsc::UnboundedReceiver<ResourceKey>,
        crate::store::OnInvalidatedFn,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let cb: crate::store::OnInvalidatedFn = Arc::new(move |key: &ResourceKey| {
            tx.send(key.clone()).ok();
        });
        (rx, cb)
    }

    fn drained(rx: &mut mpsc::UnboundedReceiver<ResourceKey>) -> Vec<ResourceKey> {
        let mut keys = Vec::new();
        while let Ok(key) = rx.try_recv() {
            keys.push(key);
        }
        keys
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn durable_displacement_does_not_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let disk = Arc::new(DiskAssetStore::new(dir.path(), CancelToken::never()));
        let (mut invalidations, cb) = record_invalidations();
        let cached = CachedAssets::new(disk, NonZeroUsize::new(2).unwrap(), Some(cb), false);

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(consts::LAYER_ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
        }

        // The oldest handle was displaced from the 2-slot in-memory LRU,
        // but its bytes survive on disk, so no invalidation must fire.
        assert_eq!(cached.cache.lock().len(), 2);
        let invalidated = drained(&mut invalidations);
        assert!(
            invalidated.is_empty(),
            "durable backend must treat LRU displacement as transparent, got {invalidated:?}"
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
            crate::test_pools::pools(),
        ));
        let (mut invalidations, cb) = record_invalidations();
        let cached = CachedAssets::new(mem, NonZeroUsize::new(2).unwrap(), Some(cb), true);

        let keys: Vec<ResourceKey> = (0..3)
            .map(|i| ResourceKey::relative(consts::LAYER_ROOT, format!("seg_{i}.m4s")))
            .collect();
        for key in &keys {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
        }

        // Ephemeral backing frees bytes when the last handle drops, so
        // displacement is real data loss and must invalidate the key.
        assert_eq!(cached.cache.lock().len(), 2);
        assert_eq!(
            drained(&mut invalidations),
            [keys[0].clone()],
            "ephemeral backend must invalidate the displaced key"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn byte_limit_is_enforced_when_retained_resource_is_released() {
        let mem = Arc::new(MemAssetStore::new(
            CancelToken::never(),
            None,
            crate::test_pools::pools(),
        ));
        let (mut invalidations, cb) = record_invalidations();
        let cached = CachedAssets::with_max_bytes(
            mem,
            NonZeroUsize::new(5).unwrap(),
            Some(cb),
            true,
            Some(3),
        );
        let key = ResourceKey::relative(consts::LAYER_ROOT, "retained.mp3");
        let writer = pending(cached.acquire_resource(&key, None).unwrap()).retain();
        writer.write_at(0, b"data").unwrap();

        let reader = writer.commit(Some(4)).unwrap();
        assert_eq!(cached.cache.lock().len(), 1);
        assert!(drained(&mut invalidations).is_empty());

        let reader = reader.release();
        assert!(cached.cache.lock().is_empty());
        assert_eq!(drained(&mut invalidations), [key]);
        drop(reader);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn pinned_resources_survive_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let cached = make_cached(dir.path(), cap);

        let keys: Vec<ResourceKey> = (0..5)
            .map(|i| ResourceKey::relative(consts::LAYER_ROOT, format!("seg_{i}.m4s")))
            .collect();

        let first = pending(cached.acquire_resource(&keys[0], None).unwrap()).retain();
        first.write_at(0, b"data").unwrap();
        first.commit(Some(4)).unwrap();

        for key in &keys[1..] {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
        }

        assert_eq!(cached.cache.lock().len(), 4);
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
            .map(|i| ResourceKey::relative(consts::LAYER_ROOT, format!("seg_{i}.m4s")))
            .collect();

        let first = pending(cached.acquire_resource(&keys[0], None).unwrap()).retain();
        first.write_at(0, b"data").unwrap();
        let first = first.commit(Some(4)).unwrap();

        for key in &keys[1..4] {
            commit_writer(cached.acquire_resource(key, None).unwrap(), b"data");
        }
        assert_eq!(cached.cache.lock().len(), 4, "3 normal + 1 pinned");

        first.release();

        commit_writer(cached.acquire_resource(&keys[4], None).unwrap(), b"data");

        assert_eq!(
            cached.cache.lock().len(),
            3,
            "released resource must be evicted, cache back to capacity"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn cache_hit_returns_same_resource() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");

        let writer = pending(cached.acquire_resource(&key, None).unwrap());
        let AcquisitionResult::Joined(joined) = cached.acquire_resource(&key, None).unwrap() else {
            panic!("a second acquire must join the live writer");
        };

        assert_eq!(writer.reader().path(), joined.path());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn concurrent_opens_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);
        let key = ResourceKey::relative(consts::LAYER_ROOT, "audio.mp3");

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

        let key = ResourceKey::relative(consts::LAYER_ROOT, "delete_me.mp3");
        let _res = pending(cached.acquire_resource(&key, None).unwrap());

        assert!(!cached.cache.lock().is_empty());

        cached.delete_asset(consts::LAYER_ROOT).unwrap();

        assert_eq!(cached.cache.lock().len(), 0);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn remove_resource_clears_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cap = NonZeroUsize::new(5).unwrap();
        let cached = make_cached(dir.path(), cap);

        let key = ResourceKey::relative(consts::LAYER_ROOT, "remove_me.mp3");
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
        let key = ResourceKey::relative(consts::LAYER_ROOT, "committed.m4s");

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

        let key_a = ResourceKey::relative(consts::LAYER_ROOT, "a.m4s");
        commit_writer(cached.acquire_resource(&key_a, None).unwrap(), b"aaaa");

        let key_b = ResourceKey::relative(consts::LAYER_ROOT, "b.m4s");
        commit_writer(cached.acquire_resource(&key_b, None).unwrap(), b"bbbb");

        let key_c = ResourceKey::relative(consts::LAYER_ROOT, "c.m4s");
        commit_writer(cached.acquire_resource(&key_c, None).unwrap(), b"cccc");

        let a_path = dir.path().join(consts::LAYER_ROOT).join("a.m4s");
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

        let key = ResourceKey::relative(consts::LAYER_ROOT, "concurrent.m4s");
        let key2 = key.clone();
        let cached2 = Arc::clone(&cached);

        let h = thread::spawn(move || {
            let _res = cached2.acquire_resource(&key2, None).unwrap();
        });

        let _res = cached.acquire_resource(&key, None).unwrap();
        h.join().unwrap();

        assert_eq!(cached.cache.lock().len(), 1);
    }
}
