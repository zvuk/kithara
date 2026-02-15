#![forbid(unsafe_code)]

//! Processing layer for resources.
//!
//! Processes content chunk-by-chunk on commit, writes to disk.
//! Uses buffer pool — no allocations during processing.

use std::{fmt::Debug, hash::Hash, ops::Range, path::Path, sync::Arc};

use kithara_bufpool::BytePool;
use kithara_storage::{ResourceExt, ResourceStatus, StorageError, StorageResult, WaitOutcome};
use parking_lot::Mutex;

use crate::{AssetsResult, ResourceKey, base::Assets};

/// Chunk size for streaming processing (64KB, multiple of AES block size 16).
const PROCESS_CHUNK_SIZE: usize = 64 * 1024;

/// Chunk-based transform function for streaming processing.
///
/// Processes data in chunks without allocating new buffers.
/// Suitable for AES-128-CBC and similar block ciphers.
///
/// The context is passed as `&mut Ctx` so stateful transforms (e.g., CBC IV chaining)
/// can update their state between chunks.
///
/// # Arguments
/// - `input`: source bytes to process
/// - `output`: buffer to write processed bytes into (same size as input)
/// - `ctx`: mutable processing context (e.g., encryption key + IV for CBC chaining)
/// - `is_last`: true if this is the final chunk (for PKCS7 padding)
///
/// # Returns
/// Number of bytes written to output buffer.
pub type ProcessChunkFn<Ctx> =
    Arc<dyn Fn(&[u8], &mut [u8], &mut Ctx, bool) -> Result<usize, String> + Send + Sync>;

/// A resource wrapper that processes content on commit.
///
/// On `commit`:
/// 1. Reads raw content in chunks
/// 2. Transforms each chunk via callback (no allocation)
/// 3. Writes processed chunks back to disk
///
/// `read_at` returns data directly from disk (already processed).
///
/// Processing only happens when `ctx` is `Some`. When `ctx` is `None`
/// (playlists, keys), commit just delegates to inner — no processing.
pub struct ProcessedResource<R, Ctx> {
    inner: R,
    ctx: Option<Ctx>,
    process: ProcessChunkFn<Ctx>,
    pool: BytePool,
    processed: Arc<Mutex<bool>>,
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

impl<R, Ctx> Debug for ProcessedResource<R, Ctx>
where
    R: Debug,
    Ctx: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessedResource")
            .field("inner", &self.inner)
            .field("ctx", &self.ctx)
            .field("is_processed", &*self.processed.lock())
            .finish_non_exhaustive()
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: Debug,
    Ctx: Clone + Debug,
{
    pub fn new(inner: R, ctx: Option<Ctx>, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        Self {
            inner,
            ctx,
            process,
            pool,
            processed: Arc::new(Mutex::new(false)),
        }
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: ResourceExt + Send + Sync + Clone + Debug + 'static,
    Ctx: Clone + Send + Sync + Debug,
{
    /// Process content chunk-by-chunk and write back to disk.
    ///
    /// Returns the total number of bytes written after processing.
    /// This may be less than `final_len` when the processor removes padding
    /// (e.g. PKCS7 unpadding in AES-128-CBC decryption).
    fn process_and_write(&self, final_len: u64) -> StorageResult<u64> {
        let Some(ctx) = &self.ctx else {
            return Ok(final_len);
        };

        // Clone context so the process function can mutate it between chunks
        // (e.g., AES-CBC IV chaining: each chunk updates IV to last ciphertext block).
        let mut ctx = ctx.clone();

        let mut input_buf = self.pool.get_with(|b| b.resize(PROCESS_CHUNK_SIZE, 0));
        let mut output_buf = self.pool.get_with(|b| b.resize(PROCESS_CHUNK_SIZE, 0));

        let chunk_size = PROCESS_CHUNK_SIZE;
        let mut read_offset = 0u64;
        let mut write_offset = 0u64;

        while read_offset < final_len {
            let remaining = (final_len - read_offset) as usize;
            let to_read = remaining.min(chunk_size);
            let is_last = read_offset + to_read as u64 >= final_len;

            let n = self.inner.read_at(read_offset, &mut input_buf[..to_read])?;
            if n == 0 {
                break;
            }

            let written = (self.process)(&input_buf[..n], &mut output_buf[..n], &mut ctx, is_last)
                .map_err(StorageError::Failed)?;

            self.inner.write_at(write_offset, &output_buf[..written])?;

            read_offset += n as u64;
            write_offset += written as u64;
        }

        Ok(write_offset)
    }
}

impl<R, Ctx> ResourceExt for ProcessedResource<R, Ctx>
where
    R: ResourceExt + Send + Sync + Clone + Debug + 'static,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.inner.wait_range(range)
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        // Process on commit (once) if ctx is present.
        // Use the actual processed length (may differ due to padding removal).
        let actual_len = {
            let mut processed = self.processed.lock();
            if !*processed && self.ctx.is_some() {
                if let Some(len) = final_len
                    && len > 0
                {
                    let processed_len = self.process_and_write(len)?;
                    *processed = true;
                    Some(processed_len)
                } else {
                    *processed = true;
                    final_len
                }
            } else {
                final_len
            }
        };

        self.inner.commit(actual_len)
    }

    fn fail(&self, reason: String) {
        self.inner.fail(reason);
    }

    fn path(&self) -> Option<&Path> {
        self.inner.path()
    }

    fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.inner.reactivate()
    }

    fn status(&self) -> ResourceStatus {
        self.inner.status()
    }
}

/// Decorator that applies processing to resources based on context.
///
/// When opening a resource with context (Some), wraps it in `ProcessedResource`
/// that will process on commit. Without context (None), the resource passes through
/// unprocessed.
#[derive(Clone)]
pub struct ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    inner: Arc<A>,
    process: ProcessChunkFn<Ctx>,
    pool: BytePool,
}

impl<A, Ctx> ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    pub fn new(inner: Arc<A>, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        Self {
            inner,
            process,
            pool,
        }
    }

    pub fn inner(&self) -> &A {
        &self.inner
    }
}

impl<A, Ctx> Assets for ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    type Res = ProcessedResource<A::Res, Ctx>;
    type Context = Ctx;
    type IndexRes = A::IndexRes;

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
        let inner = self.inner.open_resource(key)?;

        let processed =
            ProcessedResource::new(inner, ctx, Arc::clone(&self.process), self.pool.clone());

        Ok(processed)
    }

    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.inner.open_pins_index_resource()
    }

    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.inner.open_lru_index_resource()
    }

    fn open_coverage_index_resource(&self) -> AssetsResult<Self::IndexRes> {
        self.inner.open_coverage_index_resource()
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset()
    }

    fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()> {
        self.inner.remove_resource(key)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn test_pool() -> BytePool {
        BytePool::new(4, PROCESS_CHUNK_SIZE)
    }

    /// Simple mock resource for testing.
    /// Returns both the resource and the `TempDir` to keep the directory alive.
    fn mock_resource(content: &[u8]) -> (MmapResource, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let cancel = CancellationToken::new();

        let res = Resource::open(
            cancel,
            MmapOptions {
                path,
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .unwrap();
        res.write_at(0, content).unwrap();
        // Don't commit here - let the test control when commit happens
        (res, dir)
    }

    /// Create XOR chunk processor (no allocation).
    fn xor_chunk_processor(xor_key: u8, call_count: Arc<AtomicUsize>) -> ProcessChunkFn<()> {
        Arc::new(move |input, output, _ctx: &mut (), _is_last| {
            call_count.fetch_add(1, Ordering::SeqCst);
            for (i, &b) in input.iter().enumerate() {
                output[i] = b ^ xor_key;
            }
            Ok(input.len())
        })
    }

    #[test]
    fn test_process_on_commit() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(b"test content");
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        // Before commit - no processing
        assert_eq!(call_count.load(Ordering::SeqCst), 0);

        // Commit triggers processing
        processed
            .commit(Some(b"test content".len() as u64))
            .unwrap();
        assert!(call_count.load(Ordering::SeqCst) > 0);

        // Read processed data
        let mut buf = vec![0u8; 12];
        let n = processed.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);

        // Verify XOR was applied
        let expected: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(buf, expected);
    }

    #[test]
    fn test_process_called_once_on_multiple_commits() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(b"data");
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        // First commit
        processed.commit(Some(4)).unwrap();
        let count_after_first = call_count.load(Ordering::SeqCst);
        assert!(count_after_first > 0);

        // Second commit - should not process again
        processed.commit(Some(4)).unwrap();
        assert_eq!(call_count.load(Ordering::SeqCst), count_after_first);
    }

    #[test]
    fn test_read_at_after_processing() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0xFF, Arc::clone(&call_count));

        let content: Vec<u8> = (0..100).collect();
        let (resource, _dir) = mock_resource(&content);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        processed.commit(Some(100)).unwrap();

        // Read middle portion
        let mut buf = vec![0u8; 20];
        let n = processed.read_at(40, &mut buf).unwrap();
        assert_eq!(n, 20);

        // Verify XOR
        let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
        assert_eq!(buf, expected);
    }

    #[test]
    fn test_no_processing_without_ctx() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(b"test content");
        // ctx = None -> no processing
        let processed: ProcessedResource<MmapResource, ()> =
            ProcessedResource::new(resource, None, process_fn, test_pool());

        processed
            .commit(Some(b"test content".len() as u64))
            .unwrap();

        // Should NOT have called the process function
        assert_eq!(call_count.load(Ordering::SeqCst), 0);

        // Data should be unchanged
        let mut buf = vec![0u8; 12];
        let n = processed.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"test content");
    }
}
