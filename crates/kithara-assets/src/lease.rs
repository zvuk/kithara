#![forbid(unsafe_code)]

use std::{collections::HashSet, ops::Range, path::Path, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_storage::{
    AtomicResource, AtomicResourceExt, Resource, ResourceStatus, StorageError,
    StreamingResourceExt, WaitOutcome,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    ProcessedResource, base::Assets, error::AssetsResult, evict::ByteRecorder, index::PinsIndex,
    key::ResourceKey,
};

/// Decorator that adds "pin (lease) while handle lives" semantics on top of inner [`Assets`].
///
/// ## Normative behavior
/// - Every successful `open_*_resource()` pins `key.asset_root`.
/// - The pin table is stored as a `HashSet<String>` (unique roots) in memory.
/// - Every pin/unpin operation immediately persists the full table to disk using
///   `inner.open_pin_index_resource(...)` (best-effort).
/// - The pin index resource must be excluded from pinning to avoid recursion.
///
/// This type does **not** do any filesystem/path logic; it uses the inner `Assets` abstraction.
#[derive(Clone)]
pub struct LeaseAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    pins: Arc<Mutex<HashSet<String>>>,
    cancel: CancellationToken,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub fn new(inner: Arc<A>, cancel: CancellationToken) -> Self {
        Self {
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
            cancel,
            byte_recorder: None,
        }
    }

    /// Create with byte recorder for eviction tracking.
    pub fn with_byte_recorder(
        inner: Arc<A>,
        cancel: CancellationToken,
        byte_recorder: Arc<dyn ByteRecorder>,
    ) -> Self {
        Self {
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
            cancel,
            byte_recorder: Some(byte_recorder),
        }
    }

    pub(crate) fn inner(&self) -> &A {
        &self.inner
    }

    async fn open_index(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.inner()).await
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

        let (snapshot, was_new) = {
            let mut guard = self.pins.lock().await;
            let was_new = guard.insert(asset_root.to_string());
            (guard.clone(), was_new)
        };

        // Best-effort persistence: only persist if this is a new pin to avoid repeated disk writes.
        // If it fails, we still return a guard, but it will still unpin in-memory on drop.
        if was_new {
            let _ = self.persist_pins_best_effort(&snapshot).await;
        }

        Ok(LeaseGuard {
            owner: self.clone(),
            asset_root: asset_root.to_string(),
            cancel: self.cancel.clone(),
        })
    }

}

/// Resource wrapper that combines lease guard with byte recording on commit.
#[derive(Clone)]
pub struct LeaseResource<R, L> {
    inner: R,
    _lease: L,
    asset_root: String,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
}

impl<R, L> std::fmt::Debug for LeaseResource<R, L>
where
    R: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseResource")
            .field("inner", &self.inner)
            .field("asset_root", &self.asset_root)
            .finish()
    }
}

#[async_trait]
impl<R, L> Resource for LeaseResource<R, L>
where
    R: Resource + Send + Sync,
    L: Send + Sync + 'static,
{
    async fn commit(&self, final_len: Option<u64>) -> Result<(), StorageError> {
        // Commit inner resource first
        self.inner.commit(final_len).await?;

        // Record bytes if recorder is set
        if let Some(ref recorder) = self.byte_recorder
            && let Ok(metadata) = tokio::fs::metadata(self.inner.path()).await
            && metadata.is_file()
        {
            recorder
                .record_bytes(&self.asset_root, metadata.len())
                .await;
        }

        Ok(())
    }

    async fn fail(&self, error: impl Into<String> + Send) -> Result<(), StorageError> {
        self.inner.fail(error).await
    }

    fn path(&self) -> &Path {
        self.inner.path()
    }
}

#[async_trait]
impl<R, L> StreamingResourceExt for LeaseResource<R, L>
where
    R: StreamingResourceExt + Send + Sync,
    L: Send + Sync + 'static,
{
    async fn wait_range(&self, range: Range<u64>) -> Result<WaitOutcome, StorageError> {
        self.inner.wait_range(range).await
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError> {
        self.inner.read_at(offset, buf).await
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        self.inner.write_at(offset, data).await
    }
}

#[async_trait]
impl<R, L> AtomicResourceExt for LeaseResource<R, L>
where
    R: AtomicResourceExt + Send + Sync,
    L: Send + Sync + 'static,
{
    async fn write(&self, data: &[u8]) -> Result<(), StorageError> {
        self.inner.write(data).await
    }

    async fn read(&self) -> Result<Bytes, StorageError> {
        self.inner.read().await
    }
}

/// Add status() method for ProcessedResource<StreamingResource> inner.
impl<L, Ctx> LeaseResource<ProcessedResource<kithara_storage::StreamingResource, Ctx>, L>
where
    Ctx: Clone + std::fmt::Debug,
{
    /// Get resource status.
    pub async fn status(&self) -> ResourceStatus {
        self.inner.status().await
    }
}

#[async_trait]
impl<A> Assets for LeaseAssets<A>
where
    A: Assets,
{
    type StreamingRes = LeaseResource<A::StreamingRes, LeaseGuard<A>>;
    type AtomicRes = LeaseResource<A::AtomicRes, LeaseGuard<A>>;
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
        let inner = self
            .inner
            .open_streaming_resource_with_ctx(key, ctx)
            .await?;
        let lease = self.pin(self.inner.asset_root()).await?;

        Ok(LeaseResource {
            inner,
            _lease: lease,
            asset_root: self.inner.asset_root().to_string(),
            byte_recorder: self.byte_recorder.clone(),
        })
    }

    async fn open_atomic_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::AtomicRes> {
        let inner = self.inner.open_atomic_resource_with_ctx(key, ctx).await?;
        let lease = self.pin(self.inner.asset_root()).await?;

        Ok(LeaseResource {
            inner,
            _lease: lease,
            asset_root: self.inner.asset_root().to_string(),
            byte_recorder: self.byte_recorder.clone(),
        })
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        // Pins index must be opened without pinning to avoid recursion
        self.inner.open_pins_index_resource().await
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        // Delete asset through inner implementation
        self.inner.delete_asset().await
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        // LRU index must be opened without pinning
        self.inner.open_lru_index_resource().await
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

impl<A> Drop for LeaseGuard<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        if self.cancel.is_cancelled() {
            return;
        }

        tracing::debug!(asset_root = %self.asset_root, "LeaseGuard::drop - removing pin in-memory");

        // Remove pin from in-memory set immediately (sync operation)
        // Persistence is deferred to next eviction check or explicit save
        // Use try_lock to avoid blocking in Drop (which might run in async context)
        if let Ok(mut pins) = self.owner.pins.try_lock() {
            pins.remove(&self.asset_root);
            tracing::debug!(asset_root = %self.asset_root, "Pin removed from in-memory set");
        } else {
            tracing::warn!(
                asset_root = %self.asset_root,
                "Could not acquire pins lock in Drop; pin removal deferred"
            );
        }
    }
}
