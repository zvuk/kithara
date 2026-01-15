#![forbid(unsafe_code)]

//! Processing layer for streaming resources.
//!
//! Buffers entire resource, transforms via callback, caches result.

use std::{future::Future, hash::Hash, ops::Range, path::Path, pin::Pin, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use kithara_storage::{Resource, StorageError, StorageResult, StreamingResourceExt, WaitOutcome};
use tokio::sync::Mutex;

use crate::{AssetsResult, ResourceKey, base::Assets};

/// Transform function signature.
///
/// Takes raw bytes and context, returns transformed bytes.
pub type ProcessFn<Ctx> = Arc<
    dyn Fn(Bytes, Ctx) -> Pin<Box<dyn Future<Output = Result<Bytes, String>> + Send>>
        + Send
        + Sync,
>;

/// A streaming resource wrapper that buffers and transforms content.
///
/// On first `read_at`:
/// 1. Reads entire inner resource
/// 2. Transforms via callback
/// 3. Buffers the result
///
/// Subsequent `read_at` calls return data from the buffer.
pub struct ProcessedResource<R, Ctx> {
    inner: R,
    ctx: Ctx,
    process: ProcessFn<Ctx>,
    buffer: Mutex<Option<Bytes>>,
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    Ctx: Clone,
{
    pub fn new(inner: R, ctx: Ctx, process: ProcessFn<Ctx>) -> Self {
        Self {
            inner,
            ctx,
            process,
            buffer: Mutex::new(None),
        }
    }

    /// Get the inner resource.
    pub fn inner(&self) -> &R {
        &self.inner
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: StreamingResourceExt + Send + Sync,
    Ctx: Clone + Send + Sync,
{
    /// Wait until the inner resource is fully available (committed with known length).
    ///
    /// This is needed because `read()` requires the resource to be committed.
    async fn wait_for_full_content(&self) -> StorageResult<()> {
        let mut offset = 0u64;
        loop {
            let outcome = self.inner.wait_range(offset..offset + 1).await?;
            match outcome {
                WaitOutcome::Eof => {
                    // Hit EOF, resource is committed with known length.
                    return Ok(());
                }
                WaitOutcome::Ready => {
                    // Data at this offset is ready, check further ahead.
                    offset = offset.saturating_add(64 * 1024);
                }
            }
        }
    }

    async fn ensure_processed(&self) -> StorageResult<Bytes> {
        {
            let buffer = self.buffer.lock().await;
            if let Some(ref data) = *buffer {
                return Ok(data.clone());
            }
        }

        // Wait for the resource to be fully available (committed).
        // This is required because read() fails with NotCommitted otherwise.
        self.wait_for_full_content().await?;

        let mut buffer = self.buffer.lock().await;

        // Double-check after re-acquiring lock.
        if let Some(ref data) = *buffer {
            return Ok(data.clone());
        }

        // 1. Read entire content (now safe because resource is committed).
        let raw = self.inner.read().await?;

        // 2. Transform
        let processed = (self.process)(raw, self.ctx.clone())
            .await
            .map_err(StorageError::Failed)?;

        // 3. Store in buffer
        *buffer = Some(processed.clone());

        Ok(processed)
    }

    /// Get processed length (requires processing if not cached).
    pub async fn processed_len(&self) -> StorageResult<u64> {
        let processed = self.ensure_processed().await?;
        Ok(processed.len() as u64)
    }
}

#[async_trait]
impl<R, Ctx> Resource for ProcessedResource<R, Ctx>
where
    R: StreamingResourceExt + Send + Sync,
    Ctx: Clone + Send + Sync + 'static,
{
    async fn write(&self, data: &[u8]) -> StorageResult<()> {
        self.inner.write(data).await
    }

    async fn read(&self) -> StorageResult<Bytes> {
        self.ensure_processed().await
    }

    async fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.inner.commit(final_len).await
    }

    async fn fail(&self, error: impl Into<String> + Send) -> StorageResult<()> {
        self.inner.fail(error).await
    }

    fn path(&self) -> &Path {
        self.inner.path()
    }
}

#[async_trait]
impl<R, Ctx> StreamingResourceExt for ProcessedResource<R, Ctx>
where
    R: StreamingResourceExt + Send + Sync,
    Ctx: Clone + Send + Sync + 'static,
{
    async fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        // Check if already processed.
        {
            let buffer = self.buffer.lock().await;
            if buffer.is_some() {
                return Ok(WaitOutcome::Ready);
            }
        }

        // Wait for inner resource. We need all bytes for processing.
        let outcome = self.inner.wait_range(range.clone()).await?;
        if outcome == WaitOutcome::Eof {
            return Ok(WaitOutcome::Eof);
        }

        Ok(WaitOutcome::Ready)
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        let processed = self.ensure_processed().await?;

        let offset_usize = offset as usize;
        if offset_usize >= processed.len() {
            return Ok(0);
        }

        let available = processed.len() - offset_usize;
        let to_copy = buf.len().min(available);
        buf[..to_copy].copy_from_slice(&processed[offset_usize..offset_usize + to_copy]);
        Ok(to_copy)
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        // Clear buffer on write since content changed.
        {
            let mut buffer = self.buffer.lock().await;
            *buffer = None;
        }
        self.inner.write_at(offset, data).await
    }
}

/// Cache key for processed resources.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct ProcessedCacheKey<Ctx: Hash + Eq> {
    resource_key: ResourceKey,
    ctx: Ctx,
}

/// Decorator that caches processed resources.
///
/// For same (ResourceKey, Ctx) returns the same ProcessedResource handle.
pub struct ProcessingAssets<A, Ctx>
where
    A: Assets,
    Ctx: Clone + Hash + Eq + Send + Sync + 'static,
{
    inner: Arc<A>,
    process: ProcessFn<Ctx>,
    cache: DashMap<ProcessedCacheKey<Ctx>, Arc<ProcessedResource<A::StreamingRes, Ctx>>>,
}

impl<A, Ctx> ProcessingAssets<A, Ctx>
where
    A: Assets,
    Ctx: Clone + Hash + Eq + Send + Sync + 'static,
{
    pub fn new(inner: Arc<A>, process: ProcessFn<Ctx>) -> Self {
        Self {
            inner,
            process,
            cache: DashMap::new(),
        }
    }

    /// Get the underlying assets store.
    pub fn inner(&self) -> &A {
        &self.inner
    }

    /// Open a processed streaming resource.
    ///
    /// If same (key, ctx) was opened before, returns cached handle.
    pub async fn open_processed(
        &self,
        key: &ResourceKey,
        ctx: Ctx,
    ) -> AssetsResult<Arc<ProcessedResource<A::StreamingRes, Ctx>>> {
        let cache_key = ProcessedCacheKey {
            resource_key: key.clone(),
            ctx: ctx.clone(),
        };

        // Check cache first
        if let Some(res) = self.cache.get(&cache_key) {
            return Ok(Arc::clone(res.value()));
        }

        // Open inner resource
        let inner = self.inner.open_streaming_resource(key).await?;

        // Create processed wrapper
        let processed = Arc::new(ProcessedResource::new(inner, ctx, Arc::clone(&self.process)));

        // Cache and return
        self.cache.insert(cache_key, Arc::clone(&processed));
        Ok(processed)
    }

    /// Open a regular (non-processed) streaming resource.
    pub async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
    ) -> AssetsResult<A::StreamingRes> {
        self.inner.open_streaming_resource(key).await
    }

    /// Clear all cached processed resources.
    pub fn clear(&self) {
        self.cache.clear();
    }
}
