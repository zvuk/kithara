#![forbid(unsafe_code)]

//! Processing layer for streaming resources.
//!
//! Buffers entire resource, transforms via callback, caches result.

use std::{future::Future, hash::Hash, ops::Range, path::Path, pin::Pin, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_storage::{
    AtomicResource, Resource, ResourceStatus, StorageError, StorageResult, StreamingResource,
    StreamingResourceExt, WaitOutcome,
};
use tokio::sync::Mutex;

use crate::{AssetsResult, base::Assets, ResourceKey};

/// Transform function signature.
///
/// Takes raw bytes and context, returns transformed bytes.
pub type ProcessFn<Ctx> = Arc<
    dyn Fn(Bytes, Ctx) -> Pin<Box<dyn Future<Output = Result<Bytes, String>> + Send>> + Send + Sync,
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
    buffer: Arc<Mutex<Option<Bytes>>>,
}

impl<R, Ctx> Clone for ProcessedResource<R, Ctx>
where
    R: Clone,
    Ctx: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            ctx: self.ctx.clone(),
            process: Arc::clone(&self.process),
            buffer: Arc::clone(&self.buffer),
        }
    }
}

impl<R, Ctx> std::fmt::Debug for ProcessedResource<R, Ctx>
where
    R: std::fmt::Debug,
    Ctx: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessedResource")
            .field("inner", &self.inner)
            .field("ctx", &self.ctx)
            .field("has_buffer", &self.buffer.try_lock().map(|b| b.is_some()).unwrap_or(false))
            .finish()
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: std::fmt::Debug,
    Ctx: Clone + std::fmt::Debug,
{
    pub fn new(inner: R, ctx: Ctx, process: ProcessFn<Ctx>) -> Self {
        Self {
            inner,
            ctx,
            process,
            buffer: Arc::new(Mutex::new(None)),
        }
    }

    /// Get the inner resource (crate-private).
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &R {
        &self.inner
    }
}

/// Специализация для StreamingResource - добавляет метод status().
impl<Ctx> ProcessedResource<StreamingResource, Ctx>
where
    Ctx: Clone + std::fmt::Debug,
{
    /// Get resource status.
    pub async fn status(&self) -> ResourceStatus {
        self.inner.status().await
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: StreamingResourceExt + Send + Sync + std::fmt::Debug,
    Ctx: Clone + Send + Sync + std::fmt::Debug,
{
    /// Wait until the inner resource is fully available (committed with known length).
    ///
    /// This is needed because `read()` requires the resource to be committed.
    #[allow(dead_code)]
    async fn wait_for_full_content(&self) -> StorageResult<()> {
        // Check if resource is empty (EOF at offset 0).
        match self.inner.wait_range(0..1).await? {
            WaitOutcome::Eof => {
                // Empty file, already committed.
                return Ok(());
            }
            WaitOutcome::Ready => {
                // File has data, need to find EOF.
            }
        }

        // Binary search for EOF position.
        // Start with small range and double until we find EOF.
        let mut low = 0u64;
        let mut high = 1u64;

        // Find upper bound: double high until wait_range returns Eof.
        loop {
            match self.inner.wait_range(high..high + 1).await? {
                WaitOutcome::Eof => {
                    // EOF is between low and high.
                    break;
                }
                WaitOutcome::Ready => {
                    // Data available at high, double it.
                    low = high;
                    high = high.saturating_mul(2).max(high + 1);
                }
            }
        }

        // Now binary search between low and high to find exact EOF.
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            match self.inner.wait_range(mid..mid + 1).await? {
                WaitOutcome::Eof => {
                    high = mid;
                }
                WaitOutcome::Ready => {
                    low = mid;
                }
            }
        }

        // At this point, resource is committed with known length.
        Ok(())
    }

    async fn ensure_processed(&self) -> StorageResult<Bytes> {
        {
            let buffer = self.buffer.lock().await;
            if let Some(ref data) = *buffer {
                return Ok(data.clone());
            }
        }

        let mut buffer = self.buffer.lock().await;

        // Double-check after re-acquiring lock.
        if let Some(ref data) = *buffer {
            return Ok(data.clone());
        }

        // Read entire content. Note: for disk resources that are already written,
        // they should be immediately readable without NotCommitted errors.
        let raw = self.inner.read().await?;

        // Transform
        let processed = (self.process)(raw, self.ctx.clone())
            .await
            .map_err(StorageError::Failed)?;

        // Store in buffer
        *buffer = Some(processed.clone());

        Ok(processed)
    }
}

#[async_trait]
impl<R, Ctx> Resource for ProcessedResource<R, Ctx>
where
    R: StreamingResourceExt + Send + Sync + std::fmt::Debug,
    Ctx: Clone + Send + Sync + std::fmt::Debug + 'static,
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
    R: StreamingResourceExt + Send + Sync + std::fmt::Debug,
    Ctx: Clone + Send + Sync + std::fmt::Debug + 'static,
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

/// Decorator that applies processing to streaming resources based on context.
///
/// Implements the Assets trait with Context = Ctx.
/// When opening a streaming resource with context, wraps it in ProcessedResource.
/// Atomic resources are passed through unchanged.
///
/// Note: This decorator does NOT cache ProcessedResource instances.
/// Caching is handled by CachedAssets decorator which caches by (ResourceKey, Context).
#[derive(Clone)]
pub struct ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + std::fmt::Debug + 'static,
{
    inner: Arc<A>,
    process: ProcessFn<Ctx>,
}

impl<A, Ctx> ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + std::fmt::Debug + 'static,
{
    pub fn new(inner: Arc<A>, process: ProcessFn<Ctx>) -> Self {
        Self {
            inner,
            process,
        }
    }

    /// Get the underlying assets store.
    pub fn inner(&self) -> &A {
        &self.inner
    }
}

#[async_trait]
impl<A, Ctx> Assets for ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + std::fmt::Debug + 'static,
{
    type StreamingRes = ProcessedResource<A::StreamingRes, Ctx>;
    type AtomicRes = A::AtomicRes;
    type Context = Ctx;

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
        // Open inner resource without context (use convenience method)
        let inner = self.inner.open_streaming_resource(key).await?;

        // Use context if provided, otherwise use Default::default()
        let ctx = ctx.unwrap_or_default();

        // Create processed wrapper (will be cached by CachedAssets)
        let processed = ProcessedResource::new(
            inner,
            ctx,
            Arc::clone(&self.process),
        );

        Ok(processed)
    }

    async fn open_atomic_resource_with_ctx(
        &self,
        key: &ResourceKey,
        _ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::AtomicRes> {
        // Atomic resources are not processed - delegate to inner
        self.inner.open_atomic_resource(key).await
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.inner.open_pins_index_resource().await
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.inner.open_lru_index_resource().await
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_storage::StreamingResource;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Simple mock streaming resource for testing.
    async fn mock_resource(content: &[u8]) -> StreamingResource {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let cancel = CancellationToken::new();

        let res = StreamingResource::open_disk(kithara_storage::DiskOptions {
            path,
            cancel,
            initial_len: None,
        })
        .await
        .unwrap();
        res.write(content).await.unwrap();
        res.commit(Some(content.len() as u64)).await.unwrap();
        res
    }

    #[tokio::test]
    async fn test_process_fn_called_once() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&call_count);

        let process_fn: ProcessFn<()> = Arc::new(move |data, _ctx| {
            let count = Arc::clone(&count_clone);
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(data)
            })
        });

        let resource = mock_resource(b"test content").await;
        let processed = ProcessedResource::new(resource, (), process_fn);

        // First read - should call process_fn
        let result1 = processed.read().await.unwrap();
        assert_eq!(result1.as_ref(), b"test content");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "First read should call process_fn"
        );

        // Second read - should NOT call process_fn (cached)
        let result2 = processed.read().await.unwrap();
        assert_eq!(result2.as_ref(), b"test content");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Second read should use cached result"
        );
    }

    #[tokio::test]
    async fn test_process_fn_called_once_via_read_at() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&call_count);

        let process_fn: ProcessFn<()> = Arc::new(move |data, _ctx| {
            let count = Arc::clone(&count_clone);
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(data)
            })
        });

        let resource = mock_resource(b"test content").await;
        let processed = ProcessedResource::new(resource, (), process_fn);

        // First read_at - should call process_fn
        let mut buf1 = vec![0u8; 4];
        let n1 = processed.read_at(0, &mut buf1).await.unwrap();
        assert_eq!(n1, 4);
        assert_eq!(&buf1, b"test");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "First read_at should call process_fn"
        );

        // Second read_at - should NOT call process_fn (cached)
        let mut buf2 = vec![0u8; 7];
        let n2 = processed.read_at(5, &mut buf2).await.unwrap();
        assert_eq!(n2, 7);
        assert_eq!(&buf2, b"content");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Second read_at should use cached result"
        );
    }

    #[tokio::test]
    async fn test_process_fn_transformation() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&call_count);

        // Process function that uppercases the content
        let process_fn: ProcessFn<()> = Arc::new(move |data, _ctx| {
            let count = Arc::clone(&count_clone);
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                let upper: Vec<u8> = data.iter().map(|b| b.to_ascii_uppercase()).collect();
                Ok(Bytes::from(upper))
            })
        });

        let resource = mock_resource(b"hello").await;
        let processed = ProcessedResource::new(resource, (), process_fn);

        let result = processed.read().await.unwrap();
        assert_eq!(result.as_ref(), b"HELLO");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }
}
