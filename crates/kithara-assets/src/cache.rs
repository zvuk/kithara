#![forbid(unsafe_code)]

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use dashmap::DashMap;
use kithara_storage::AtomicResource;

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

type Cache<S, A> = DashMap<CacheKey, CacheEntry<S, A>>;

/// A decorator that caches opened resources in memory.
///
/// ## Normative
/// - Caching is done at the resource level (not asset level).
/// - Same `ResourceKey` returns the same resource handle.
/// - Cache is process-scoped and not persisted.
#[derive(Clone, Debug)]
pub struct CachedAssets<A>
where
    A: Assets,
{
    base: Arc<A>,
    cache: Arc<Cache<A::StreamingRes, A::AtomicRes>>,
}

impl<A> CachedAssets<A>
where
    A: Assets,
{
    pub fn new(base: Arc<A>) -> Self {
        Self {
            base,
            cache: Arc::new(DashMap::new()),
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

        if let Some(entry) = self.cache.get(&cache_key)
            && let CacheEntry::Atomic(res) = entry.value()
        {
            return Ok(res.clone());
        }

        let res = self.base.open_atomic_resource(key).await?;

        match self.cache.entry(cache_key) {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if let CacheEntry::Atomic(existing) = e.get() {
                    Ok(existing.clone())
                } else {
                    Ok(res)
                }
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(CacheEntry::Atomic(res.clone()));
                Ok(res)
            }
        }
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<Self::StreamingRes> {
        let cache_key = CacheKey::Streaming(key.clone());

        if let Some(entry) = self.cache.get(&cache_key)
            && let CacheEntry::Streaming(res) = entry.value()
        {
            return Ok(res.clone());
        }

        let res = self.base.open_streaming_resource(key).await?;

        match self.cache.entry(cache_key) {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if let CacheEntry::Streaming(existing) = e.get() {
                    Ok(existing.clone())
                } else {
                    Ok(res)
                }
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(CacheEntry::Streaming(res.clone()));
                Ok(res)
            }
        }
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        if let Some(entry) = self.cache.get(&CacheKey::PinsIndex)
            && let CacheEntry::Index(res) = entry.value()
        {
            return Ok(res.clone());
        }

        let res = self.base.open_pins_index_resource().await?;

        match self.cache.entry(CacheKey::PinsIndex) {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if let CacheEntry::Index(existing) = e.get() {
                    Ok(existing.clone())
                } else {
                    Ok(res)
                }
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(CacheEntry::Index(res.clone()));
                Ok(res)
            }
        }
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        if let Some(entry) = self.cache.get(&CacheKey::LruIndex)
            && let CacheEntry::Index(res) = entry.value()
        {
            return Ok(res.clone());
        }

        let res = self.base.open_lru_index_resource().await?;

        match self.cache.entry(CacheKey::LruIndex) {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if let CacheEntry::Index(existing) = e.get() {
                    Ok(existing.clone())
                } else {
                    Ok(res)
                }
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(CacheEntry::Index(res.clone()));
                Ok(res)
            }
        }
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        // Clear streaming and atomic caches for this asset (keep index entries)
        self.cache
            .retain(|k, _| matches!(k, CacheKey::PinsIndex | CacheKey::LruIndex));

        self.base.delete_asset().await
    }
}
