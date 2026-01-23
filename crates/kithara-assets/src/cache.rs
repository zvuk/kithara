#![forbid(unsafe_code)]

use std::{num::NonZeroUsize, path::Path, sync::Arc};

use async_trait::async_trait;
use kithara_storage::AtomicResource;
use lru::LruCache;
use parking_lot::Mutex;

use crate::{base::Assets, error::AssetsResult, key::ResourceKey};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum CacheKey {
    Streaming(ResourceKey),
    Atomic(ResourceKey),
    PinsIndex,
    LruIndex,
}

#[derive(Clone, Debug)]
enum CacheEntry<S, A> {
    Streaming(S),
    Atomic(A),
    Index(AtomicResource),
}

type Cache<S, A> = Mutex<LruCache<CacheKey, CacheEntry<S, A>>>;

/// A decorator that caches opened resources in memory with LRU eviction.
///
/// ## Normative
/// - Caching is done at the resource level (not asset level).
/// - Same `ResourceKey` returns the same resource handle.
/// - Cache is process-scoped and not persisted.
/// - LRU capacity: 5 entries (enough for init + 2-3 media segments).
#[derive(Clone)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    base: Arc<A>,
    cache: Arc<Cache<A::StreamingRes, A::AtomicRes>>,
}

impl<A> std::fmt::Debug for CachedAssets<A>
where
    A: Assets,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAssets")
            .field("cache_size", &self.cache.lock().len())
            .finish()
    }
}

impl<A> CachedAssets<A>
where
    A: Assets,
{
    pub fn new(base: Arc<A>) -> Self {
        Self {
            base,
            cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(5).expect("capacity must be non-zero"),
            ))),
        }
    }

    pub fn base(&self) -> &A {
        &self.base
    }
}

#[async_trait]
impl<A> Assets for CachedAssets<A>
where
    A: Assets,
{
    type StreamingRes = A::StreamingRes;
    type AtomicRes = A::AtomicRes;

    fn root_dir(&self) -> &Path {
        self.base.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.base.asset_root()
    }

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<Self::AtomicRes> {
        let cache_key = CacheKey::Atomic(key.clone());

        // Check cache first
        {
            let mut cache = self.cache.lock();
            if let Some(CacheEntry::Atomic(res)) = cache.get(&cache_key) {
                return Ok(res.clone());
            }
        }

        // Cache miss - load from base
        let res = self.base.open_atomic_resource(key).await?;

        // Insert into cache (LRU evicts oldest if full)
        {
            let mut cache = self.cache.lock();
            cache.put(cache_key, CacheEntry::Atomic(res.clone()));
        }

        Ok(res)
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<Self::StreamingRes> {
        let cache_key = CacheKey::Streaming(key.clone());

        // Check cache first
        {
            let mut cache = self.cache.lock();
            if let Some(CacheEntry::Streaming(res)) = cache.get(&cache_key) {
                return Ok(res.clone());
            }
        }

        // Cache miss - load from base
        let res = self.base.open_streaming_resource(key).await?;

        // Insert into cache (LRU evicts oldest if full)
        {
            let mut cache = self.cache.lock();
            cache.put(cache_key, CacheEntry::Streaming(res.clone()));
        }

        Ok(res)
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        // Check cache first
        {
            let mut cache = self.cache.lock();
            if let Some(CacheEntry::Index(res)) = cache.get(&CacheKey::PinsIndex) {
                return Ok(res.clone());
            }
        }

        // Cache miss - load from base
        let res = self.base.open_pins_index_resource().await?;

        // Insert into cache
        {
            let mut cache = self.cache.lock();
            cache.put(CacheKey::PinsIndex, CacheEntry::Index(res.clone()));
        }

        Ok(res)
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        // Check cache first
        {
            let mut cache = self.cache.lock();
            if let Some(CacheEntry::Index(res)) = cache.get(&CacheKey::LruIndex) {
                return Ok(res.clone());
            }
        }

        // Cache miss - load from base
        let res = self.base.open_lru_index_resource().await?;

        // Insert into cache
        {
            let mut cache = self.cache.lock();
            cache.put(CacheKey::LruIndex, CacheEntry::Index(res.clone()));
        }

        Ok(res)
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        // Clear streaming and atomic caches for this asset (keep index entries)
        {
            let mut cache = self.cache.lock();

            // Collect keys to remove (LruCache doesn't have retain())
            let keys_to_remove: Vec<CacheKey> = cache
                .iter()
                .filter_map(|(k, _)| {
                    if !matches!(k, CacheKey::PinsIndex | CacheKey::LruIndex) {
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

        self.base.delete_asset().await
    }
}
