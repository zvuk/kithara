#![forbid(unsafe_code)]

//! Processing layer for streaming resources.
//!
//! Processes content chunk-by-chunk on commit, writes to disk.
//! Uses buffer pool — no allocations during processing.

use std::{hash::Hash, ops::Range, path::Path, sync::Arc};

use async_trait::async_trait;
use kithara_bufpool::BytePool;
use kithara_storage::{
    AtomicResource, Resource, ResourceStatus, StorageError, StorageResult, StreamingResource,
    StreamingResourceExt, WaitOutcome,
};
use tokio::sync::OnceCell;

use crate::{AssetsResult, ResourceKey, base::Assets};

/// Chunk size for streaming processing (64KB, multiple of AES block size 16).
const PROCESS_CHUNK_SIZE: usize = 64 * 1024;

/// Chunk-based transform function for streaming processing.
///
/// Processes data in chunks without allocating new buffers.
/// Suitable for AES-128-CBC and similar block ciphers.
///
/// # Arguments
/// - `input`: source bytes to process
/// - `output`: buffer to write processed bytes into (same size as input)
/// - `ctx`: processing context (e.g., encryption key, IV)
/// - `is_last`: true if this is the final chunk (for PKCS7 padding)
///
/// # Returns
/// Number of bytes written to output buffer.
pub type ProcessChunkFn<Ctx> =
    Arc<dyn Fn(&[u8], &mut [u8], &Ctx, bool) -> Result<usize, String> + Send + Sync>;

/// A streaming resource wrapper that processes content on commit.
///
/// On `commit`:
/// 1. Reads raw content in chunks
/// 2. Transforms each chunk via callback (no allocation)
/// 3. Writes processed chunks back to disk
///
/// `read_at` returns data directly from disk (already processed).
pub struct ProcessedResource<R, Ctx> {
    inner: R,
    ctx: Ctx,
    process: ProcessChunkFn<Ctx>,
    pool: BytePool,
    processed: Arc<OnceCell<()>>,
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
            pool: self.pool.clone(),
            processed: Arc::clone(&self.processed),
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
            .field("is_processed", &self.processed.get().is_some())
            .finish()
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: std::fmt::Debug,
    Ctx: Clone + std::fmt::Debug,
{
    pub fn new(inner: R, ctx: Ctx, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        Self {
            inner,
            ctx,
            process,
            pool,
            processed: Arc::new(OnceCell::new()),
        }
    }

    /// Get the inner resource (crate-private).
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &R {
        &self.inner
    }
}

/// Specialization for StreamingResource — adds status() method.
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
    /// Process content chunk-by-chunk and write back to disk.
    ///
    /// Uses pool buffers, no allocations during processing loop.
    async fn process_and_write(&self, final_len: u64) -> StorageResult<()> {
        // Get buffers from pool
        let mut input_buf = self.pool.get_with(|b| b.resize(PROCESS_CHUNK_SIZE, 0));
        let mut output_buf = self.pool.get_with(|b| b.resize(PROCESS_CHUNK_SIZE, 0));

        let chunk_size = PROCESS_CHUNK_SIZE;
        let mut offset = 0u64;

        while offset < final_len {
            let remaining = (final_len - offset) as usize;
            let to_read = remaining.min(chunk_size);
            let is_last = offset + to_read as u64 >= final_len;

            // Read chunk from disk
            let n = self
                .inner
                .read_at(offset, &mut input_buf[..to_read])
                .await?;
            if n == 0 {
                break;
            }

            // Process chunk (no allocation)
            let written = (self.process)(&input_buf[..n], &mut output_buf[..n], &self.ctx, is_last)
                .map_err(StorageError::Failed)?;

            // Write processed chunk back to disk
            self.inner.write_at(offset, &output_buf[..written]).await?;

            offset += n as u64;
        }

        Ok(())
    }
}

#[async_trait]
impl<R, Ctx> Resource for ProcessedResource<R, Ctx>
where
    R: StreamingResourceExt + Send + Sync + std::fmt::Debug,
    Ctx: Clone + Send + Sync + std::fmt::Debug + 'static,
{
    async fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        // Process on commit (once) using pool buffers
        // Do this BEFORE final commit to avoid resetting committed state
        self.processed
            .get_or_try_init(|| async {
                if let Some(len) = final_len {
                    if len > 0 {
                        self.process_and_write(len).await?;
                    }
                }
                Ok::<(), StorageError>(())
            })
            .await?;

        // Commit inner AFTER processing to set final committed state
        self.inner.commit(final_len).await?;

        Ok(())
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
        // After processing, just delegate to inner
        self.inner.wait_range(range).await
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        // After processing, data is on disk. Just delegate to inner.
        self.inner.read_at(offset, buf).await
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
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
    process: ProcessChunkFn<Ctx>,
    pool: BytePool,
}

impl<A, Ctx> ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + std::fmt::Debug + 'static,
{
    pub fn new(inner: Arc<A>, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        Self {
            inner,
            process,
            pool,
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

        // Create processed wrapper with pool
        let processed =
            ProcessedResource::new(inner, ctx, Arc::clone(&self.process), self.pool.clone());

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

    fn test_pool() -> BytePool {
        BytePool::new(4, PROCESS_CHUNK_SIZE)
    }

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
        res.write_at(0, content).await.unwrap();
        // Don't commit here - let the test control when commit happens
        res
    }

    /// Create XOR chunk processor (no allocation).
    fn xor_chunk_processor(xor_key: u8, call_count: Arc<AtomicUsize>) -> ProcessChunkFn<()> {
        Arc::new(move |input, output, _ctx, _is_last| {
            call_count.fetch_add(1, Ordering::SeqCst);
            for (i, &b) in input.iter().enumerate() {
                output[i] = b ^ xor_key;
            }
            Ok(input.len())
        })
    }

    #[tokio::test]
    async fn test_process_on_commit() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));

        let resource = mock_resource(b"test content").await;
        let processed = ProcessedResource::new(resource, (), process_fn, test_pool());

        // Before commit - no processing
        assert_eq!(call_count.load(Ordering::SeqCst), 0);

        // Commit triggers processing
        processed
            .commit(Some(b"test content".len() as u64))
            .await
            .unwrap();
        assert!(call_count.load(Ordering::SeqCst) > 0);

        // Read processed data
        let mut buf = vec![0u8; 12];
        let n = processed.read_at(0, &mut buf).await.unwrap();
        assert_eq!(n, 12);

        // Verify XOR was applied
        let expected: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(buf, expected);
    }

    #[tokio::test]
    async fn test_process_called_once_on_multiple_commits() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

        let resource = mock_resource(b"data").await;
        let processed = ProcessedResource::new(resource, (), process_fn, test_pool());

        // First commit
        processed.commit(Some(4)).await.unwrap();
        let count_after_first = call_count.load(Ordering::SeqCst);
        assert!(count_after_first > 0);

        // Second commit - should not process again
        processed.commit(Some(4)).await.unwrap();
        assert_eq!(call_count.load(Ordering::SeqCst), count_after_first);
    }

    #[tokio::test]
    async fn test_read_at_after_processing() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0xFF, Arc::clone(&call_count));

        let content: Vec<u8> = (0..100).collect();
        let resource = mock_resource(&content).await;
        let processed = ProcessedResource::new(resource, (), process_fn, test_pool());

        processed.commit(Some(100)).await.unwrap();

        // Read middle portion
        let mut buf = vec![0u8; 20];
        let n = processed.read_at(40, &mut buf).await.unwrap();
        assert_eq!(n, 20);

        // Verify XOR
        let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
        assert_eq!(buf, expected);
    }
}
