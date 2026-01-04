#![forbid(unsafe_code)]

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kithara_storage::{AtomicResource, Resource, StreamingResource};
use tokio_util::sync::CancellationToken;

use crate::{
    cache::Assets, error::CacheResult, index::AssetIndex, key::ResourceKey, resource::AssetResource,
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
pub struct LeaseAssets<A> {
    base: Arc<A>,
    pins: Arc<Mutex<HashSet<String>>>,
}

impl<A> Clone for LeaseAssets<A> {
    fn clone(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            pins: Arc::clone(&self.pins),
        }
    }
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub fn new(base: Arc<A>) -> Self {
        Self {
            base,
            pins: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn base(&self) -> &A {
        &self.base
    }

    async fn persist_pins_best_effort(
        &self,
        cancel: CancellationToken,
        pins: &HashSet<String>,
    ) -> CacheResult<()> {
        // Persist as AssetIndex, but we treat it as *pin index only* for now.
        // The rest of fields are best-effort placeholders.
        let index = AssetIndex {
            version: 1,
            resources: Vec::new(),
            pinned: pins.iter().cloned().collect(),
        };

        let res = self
            .base
            .open_static_meta_resource(&ResourceKey::new("_index", "pins.json"), cancel)
            .await?;
        let bytes = serde_json::to_vec_pretty(&index)?;
        res.write(&bytes).await?;
        Ok(())
    }

    async fn load_pins_best_effort(
        &self,
        cancel: CancellationToken,
    ) -> CacheResult<HashSet<String>> {
        let res = self
            .base
            .open_static_meta_resource(&ResourceKey::new("_index", "pins.json"), cancel)
            .await?;
        let bytes = res.read().await?;

        if bytes.is_empty() {
            return Ok(HashSet::new());
        }

        let index: AssetIndex = serde_json::from_slice(&bytes)?;
        Ok(index.pinned.into_iter().collect())
    }

    async fn ensure_loaded_best_effort(&self, cancel: CancellationToken) -> CacheResult<()> {
        // Load only once (best-effort). If already loaded, do nothing.
        //
        // We intentionally keep this minimal and deterministic: if the set is empty
        // we may still be "loaded but empty", but that is acceptable for now because
        // higher layers can repin when they open resources.
        let should_load = { self.pins.lock().expect("pins mutex poisoned").is_empty() };

        if !should_load {
            return Ok(());
        }

        let loaded = self.load_pins_best_effort(cancel).await?;
        let mut guard = self.pins.lock().expect("pins mutex poisoned");
        // Merge, don't replace, to be resilient to races with concurrent pin/unpin.
        for p in loaded {
            guard.insert(p);
        }
        Ok(())
    }

    async fn pin(&self, asset_root: &str, cancel: CancellationToken) -> CacheResult<LeaseGuard<A>> {
        self.ensure_loaded_best_effort(cancel.clone()).await?;

        let snapshot = {
            let mut guard = self.pins.lock().expect("pins mutex poisoned");
            guard.insert(asset_root.to_string());
            guard.clone()
        };

        // Best-effort persistence: if it fails, we still return a guard, but it will still unpin
        // in-memory on drop. The disk state may lag, but opening resources continues to work.
        let _ = self
            .persist_pins_best_effort(cancel.clone(), &snapshot)
            .await;

        Ok(LeaseGuard {
            owner: self.clone(),
            asset_root: asset_root.to_string(),
            cancel,
        })
    }

    async fn unpin_best_effort(&self, asset_root: &str, cancel: CancellationToken) {
        let snapshot = {
            let mut guard = self.pins.lock().expect("pins mutex poisoned");
            guard.remove(asset_root);
            guard.clone()
        };

        let _ = self.persist_pins_best_effort(cancel, &snapshot).await;
    }

    /// Open an atomic resource via the base store and wrap it into [`AssetResource`]
    /// holding an RAII lease guard.
    pub async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<AtomicResource, LeaseGuard<A>>> {
        let inner = self.base.open_atomic_resource(key, cancel.clone()).await?;

        let lease = self.pin(&key.asset_root, cancel).await?;
        Ok(AssetResource::new(inner, lease))
    }

    /// Open a streaming resource via the base store and wrap it into [`AssetResource`]
    /// holding an RAII lease guard.
    pub async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<StreamingResource, LeaseGuard<A>>> {
        let inner = self
            .base
            .open_streaming_resource(key, cancel.clone())
            .await?;

        let lease = self.pin(&key.asset_root, cancel).await?;
        Ok(AssetResource::new(inner, lease))
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
            owner.unpin_best_effort(&asset_root, cancel).await;
        });
    }
}

/// Trait used by callers that want leased (auto-pinning) behavior.
///
/// This exists to keep the public contract readable and avoid exposing internal mechanics.
///
/// Note: we return `AssetResource<_, _>` with an *opaque lease type* (`impl Send + Sync + 'static`)
/// so callers don't depend on the concrete guard type.
#[async_trait]
pub trait LeaseAssetsExt: Send + Sync + 'static {
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<AtomicResource, impl Send + Sync + 'static>>;

    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<StreamingResource, impl Send + Sync + 'static>>;
}

#[async_trait]
impl<A> LeaseAssetsExt for LeaseAssets<A>
where
    A: Assets,
{
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<AtomicResource, impl Send + Sync + 'static>> {
        LeaseAssets::open_atomic_resource(self, key, cancel).await
    }

    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> CacheResult<AssetResource<StreamingResource, impl Send + Sync + 'static>> {
        LeaseAssets::open_streaming_resource(self, key, cancel).await
    }
}
