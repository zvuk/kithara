#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, path::Path, sync::Arc};

use async_trait::async_trait;
use kithara_storage::AtomicResource;
use lru::LruCache;
use tokio::sync::Mutex;

use crate::{base::Assets, error::AssetsResult, key::ResourceKey};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum CacheKey<C> {
    Streaming(ResourceKey, Option<C>),
    Atomic(ResourceKey, Option<C>),
    PinsIndex,
    LruIndex,
}

#[derive(Clone, Debug)]
enum CacheEntry<S, A> {
    Streaming(S),
    Atomic(A),
    Index(AtomicResource),
}

type Cache<S, A, C> = Mutex<LruCache<CacheKey<C>, CacheEntry<S, A>>>;

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
    cache: Arc<Cache<A::StreamingRes, A::AtomicRes, A::Context>>,
}

impl<A> std::fmt::Debug for CachedAssets<A>
where
    A: Assets,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Can't await in Debug::fmt, so try_lock instead
        let size = self.cache.try_lock().map(|c| c.len()).ok();
        f.debug_struct("CachedAssets")
            .field("cache_size", &size)
            .finish()
    }
}

impl<A> CachedAssets<A>
where
    A: Assets,
{
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

#[async_trait]
impl<A> Assets for CachedAssets<A>
where
    A: Assets,
{
    type StreamingRes = A::StreamingRes;
    type AtomicRes = A::AtomicRes;
    type Context = A::Context;

    fn root_dir(&self) -> &Path {
        self.inner.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.inner.asset_root()
    }

    async fn open_streaming_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::StreamingRes> {
        let cache_key = CacheKey::Streaming(key.clone(), ctx.clone());

        // Hold lock across async call to prevent race condition
        // where two callers both miss cache and create separate resources
        let mut cache = self.cache.lock().await;

        if let Some(CacheEntry::Streaming(res)) = cache.get(&cache_key) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self
            .inner
            .open_streaming_resource_with_ctx(key, ctx)
            .await?;

        // Insert into cache (LRU evicts oldest if full)
        cache.put(cache_key, CacheEntry::Streaming(res.clone()));

        Ok(res)
    }

    async fn open_atomic_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::AtomicRes> {
        let cache_key = CacheKey::Atomic(key.clone(), ctx.clone());

        // Hold lock across async call to prevent race condition
        let mut cache = self.cache.lock().await;

        if let Some(CacheEntry::Atomic(res)) = cache.get(&cache_key) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self.inner.open_atomic_resource_with_ctx(key, ctx).await?;

        // Insert into cache (LRU evicts oldest if full)
        cache.put(cache_key, CacheEntry::Atomic(res.clone()));

        Ok(res)
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        // Hold lock across async call to prevent race condition
        let mut cache = self.cache.lock().await;

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::PinsIndex) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self.inner.open_pins_index_resource().await?;

        // Insert into cache
        cache.put(CacheKey::PinsIndex, CacheEntry::Index(res.clone()));

        Ok(res)
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        // Hold lock across async call to prevent race condition
        let mut cache = self.cache.lock().await;

        if let Some(CacheEntry::Index(res)) = cache.peek(&CacheKey::LruIndex) {
            return Ok(res.clone());
        }

        // Cache miss - load from base (still holding lock)
        let res = self.inner.open_lru_index_resource().await?;

        // Insert into cache
        cache.put(CacheKey::LruIndex, CacheEntry::Index(res.clone()));

        Ok(res)
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        // Clear streaming and atomic caches for this asset (keep index entries)
        {
            let mut cache = self.cache.lock().await;

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

        self.inner.delete_asset().await
    }
}
