#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, path::Path, sync::Arc};

use kithara_storage::StorageResource;
use lru::LruCache;
use parking_lot::Mutex;

use crate::{base::Assets, error::AssetsResult, key::ResourceKey};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum CacheKey<C> {
    Resource(ResourceKey, Option<C>),
    PinsIndex,
    LruIndex,
}

#[derive(Clone, Debug)]
enum CacheEntry<R> {
    Resource(R),
    Index(StorageResource),
}

type Cache<R, C> = Mutex<LruCache<CacheKey<C>, CacheEntry<R>>>;

/// A decorator that caches opened resources in memory with LRU eviction.
///
/// ## Normative
/// - Caching is done at the resource level (not asset level).
/// - Same `(ResourceKey, Context)` returns the same resource handle.
/// - Cache is process-scoped and not persisted.
/// - LRU capacity: 5 entries (enough for init + 2-3 media segments).
#[derive(Clone)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    cache: Arc<Cache<A::Res, A::Context>>,
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
    /// # Panics
    /// This function will not panic (capacity is a non-zero constant).
    pub fn new(inner: Arc<A>) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(5).expect("capacity must be non-zero"),
            ))),
        }
    }

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
        let cache_key = CacheKey::Resource(key.clone(), ctx.clone());

        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Resource(res)) = cache.get(&cache_key) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self.inner.open_resource_with_ctx(key, ctx)?;

        // Insert into cache (LRU evicts oldest if full)
        cache.put(cache_key, CacheEntry::Resource(res.clone()));

        Ok(res)
    }

    fn open_pins_index_resource(&self) -> AssetsResult<StorageResource> {
        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::PinsIndex) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self.inner.open_pins_index_resource()?;

        // Insert into cache
        cache.put(CacheKey::PinsIndex, CacheEntry::Index(res.clone()));

        Ok(res)
    }

    fn open_lru_index_resource(&self) -> AssetsResult<StorageResource> {
        let mut cache = self.cache.lock();

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::LruIndex) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self.inner.open_lru_index_resource()?;

        // Insert into cache
        cache.put(CacheKey::LruIndex, CacheEntry::Index(res.clone()));

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
}
