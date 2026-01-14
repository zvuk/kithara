#![forbid(unsafe_code)]

use std::{collections::HashSet, path::Path, sync::Arc};

use async_trait::async_trait;
use kithara_storage::{AtomicResource, StreamingResource};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    cache::Assets, error::AssetsResult, index::PinsIndex, key::ResourceKey, resource::AssetResource,
};

/// Decorator that adds "pin (lease) while handle lives" semantics on top of a base [`Assets`].
///
/// ## Normative behavior
/// - Every successful `open_*_resource()` pins `key.asset_root`.
/// - The pin table is stored as a `HashSet<String>` (unique roots) in memory.
/// - Every pin/unpin operation immediately persists the full table to disk using
///   `base.open_pin_index_resource(...)` (best-effort).
/// - The pin index resource must be excluded from pinning to avoid recursion.
///
/// This type does **not** do any filesystem/path logic; it uses the base `Assets` abstraction.
#[derive(Clone, Debug)]
pub struct LeaseAssets<A>
where
    A: Assets,
{
    base: Arc<A>,
    pins: Arc<Mutex<HashSet<String>>>,
    cancel: CancellationToken,
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub fn new(base: Arc<A>, cancel: CancellationToken) -> Self {
        Self {
            base,
            pins: Arc::new(Mutex::new(HashSet::new())),
            cancel,
        }
    }

    pub fn base(&self) -> &A {
        &self.base
    }

    async fn open_index(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.base()).await
    }

    async fn persist_pins_best_effort(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let idx = self.open_index().await?;
        idx.store(pins).await
    }

    async fn load_pins_best_effort(&self) -> AssetsResult<HashSet<String>> {
        let idx = self.open_index().await?;
        idx.load().await
    }

    async fn ensure_loaded_best_effort(&self) -> AssetsResult<()> {
        // Load only once (best-effort). If already loaded, do nothing.
        //
        // We intentionally keep this minimal and deterministic: if the set is empty
        // we may still be "loaded but empty", but that is acceptable for now because
        // higher layers can repin when they open resources.
        let should_load = { self.pins.lock().await.is_empty() };

        if !should_load {
            return Ok(());
        }

        let loaded = self.load_pins_best_effort().await?;
        let mut guard = self.pins.lock().await;
        // Merge, don't replace, to be resilient to races with concurrent pin/unpin.
        for p in loaded {
            guard.insert(p);
        }
        Ok(())
    }

    async fn pin(&self, asset_root: &str) -> AssetsResult<LeaseGuard<A>> {
        self.ensure_loaded_best_effort().await?;

        let snapshot = {
            let mut guard = self.pins.lock().await;
            guard.insert(asset_root.to_string());
            guard.clone()
        };

        // Best-effort persistence: if it fails, we still return a guard, but it will still unpin
        // in-memory on drop. The disk state may lag, but opening resources continues to work.
        let _ = self.persist_pins_best_effort(&snapshot).await;

        Ok(LeaseGuard {
            owner: self.clone(),
            asset_root: asset_root.to_string(),
            cancel: self.cancel.clone(),
        })
    }

    async fn unpin_best_effort(&self, asset_root: &str) {
        let snapshot = {
            let mut guard = self.pins.lock().await;
            guard.remove(asset_root);
            guard.clone()
        };

        let _ = self.persist_pins_best_effort(&snapshot).await;
    }

    /// Open an atomic resource via the base store and wrap it into [`AssetResource`]
    /// holding an RAII lease guard.
    pub async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
    ) -> AssetsResult<AssetResource<AtomicResource, LeaseGuard<A>>> {
        let inner = self.base.open_atomic_resource(key).await?;

        let lease = self.pin(self.base.asset_root()).await?;
        Ok(AssetResource::new(inner, lease))
    }

    /// Open a streaming resource via the base store and wrap it into [`AssetResource`]
    /// holding an RAII lease guard.
    pub async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
    ) -> AssetsResult<AssetResource<StreamingResource, LeaseGuard<A>>> {
        let inner = self.base.open_streaming_resource(key).await?;

        let lease = self.pin(self.base.asset_root()).await?;
        Ok(AssetResource::new(inner, lease))
    }
}

#[async_trait]
impl<A> Assets for LeaseAssets<A>
where
    A: Assets,
{
    fn root_dir(&self) -> &Path {
        self.base.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.base.asset_root()
    }

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<AtomicResource> {
        // Note: This method bypasses pinning and returns a raw resource.
        // The public `open_atomic_resource` method wraps it with a lease guard.
        self.base.open_atomic_resource(key).await
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<StreamingResource> {
        // Note: This method bypasses pinning and returns a raw resource.
        // The public `open_streaming_resource` method wraps it with a lease guard.
        self.base.open_streaming_resource(key).await
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        // Pins index must be opened without pinning to avoid recursion
        self.base.open_pins_index_resource().await
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        // Delete asset through base implementation
        self.base.delete_asset().await
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        // LRU index must be opened without pinning
        self.base.open_lru_index_resource().await
    }
}

/// RAII guard for a pin.
///
/// Dropping this guard unpins the corresponding `asset_root` and persists the new pin set
/// to disk (best-effort) via the decorator.
#[derive(Clone)]
pub struct LeaseGuard<A>
where
    A: Assets,
{
    owner: LeaseAssets<A>,
    asset_root: String,
    cancel: CancellationToken,
}

impl<A> LeaseGuard<A>
where
    A: Assets,
{
    pub fn asset_root(&self) -> &str {
        &self.asset_root
    }
}

impl<A> Drop for LeaseGuard<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        let owner = self.owner.clone();
        let asset_root = self.asset_root.clone();
        let cancel = self.cancel.clone();

        tokio::spawn(async move {
            if cancel.is_cancelled() {
                return;
            }
            owner.unpin_best_effort(&asset_root).await;
        });
    }
}
