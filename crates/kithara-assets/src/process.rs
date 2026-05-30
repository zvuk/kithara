#![forbid(unsafe_code)]

use std::{
    fmt,
    fmt::Debug,
    hash::Hash,
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::BytePool;
use kithara_platform::{
    Condvar, Mutex,
    time::{Duration, Instant},
};
use kithara_storage::{ResourceStatus, StorageError, StorageResult, WaitOutcome};

use crate::{
    AssetResourceState, AssetsResult, ResourceKey,
    acquisition::{ReadSide, WriteSide},
    base::{Assets, ResourceHandle},
    identity::RequestIdentity,
};

/// Constants for streaming processing (64KB, multiple of AES block size 16).
struct Consts;

impl Consts {
    /// Chunk size for streaming processing (64KB, multiple of AES block size 16).
    const CHUNK_SIZE: usize = 64 * 1024;
    /// `CHUNK_SIZE` mirrored as u64 so chunked file-offset arithmetic can
    /// stay in the wider type and only narrow once the value is known to fit.
    const CHUNK_SIZE_U64: u64 = 64 * 1024;
}

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

/// A resource wrapper that transforms content on `commit` (e.g. DRM decrypt).
///
/// `read_at` returns already-processed bytes; processing runs only when
/// `ctx` is `Some`. A [`ReadinessGate`] keeps readers from observing
/// "ready" bytes before `commit` finishes. See the crate `README.md`
/// "Processing & readiness gate".
pub struct ProcessedResource<R, Ctx> {
    readiness: Arc<ReadinessGate>,
    pool: BytePool,
    ctx: Option<Ctx>,
    process: ProcessChunkFn<Ctx>,
    inner: R,
}

/// Pairs a `processed` flag with a [`Condvar`] so readers can block
/// until [`ProcessedResource::commit`] drains the processor.
///
/// Splitting this out keeps the locking discipline explicit: the
/// guard is only ever held inside [`ReadinessGate::wait_until_ready`]
/// or the brief flip in [`ReadinessGate::mark_ready`].
struct ReadinessGate {
    cv: Condvar,
    processed: Mutex<bool>,
    failed: AtomicBool,
}

impl ReadinessGate {
    fn new(initial: bool) -> Self {
        Self {
            processed: Mutex::new(initial),
            cv: Condvar::new(),
            failed: AtomicBool::new(false),
        }
    }

    fn is_ready(&self) -> bool {
        *self.processed.lock_sync()
    }

    fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Fail the gate: wake every waiter so a writer dropped without
    /// `commit` cannot deadlock a reader parked in [`Self::wait_until_ready`].
    /// Waiters observe [`Self::is_failed`] and abort. `processed` is left
    /// untouched so [`Self::is_ready`] stays `false` — a failed (never
    /// committed) resource must not read as valid via `read_at`.
    fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    /// Reset to "not ready" — used by [`ProcessedResource::reactivate`]
    /// when the inner resource is reopened for fresh writes.
    fn mark_pending(&self) {
        *self.processed.lock_sync() = false;
    }

    /// Mark the gate ready and wake every waiter.
    fn mark_ready(&self) {
        *self.processed.lock_sync() = true;
        self.cv.notify_all();
    }

    /// Block the caller until `processed` becomes `true` or
    /// `should_abort` reports that the underlying resource has
    /// failed/cancelled. Returns `true` if the gate was reached,
    /// `false` if the wait was aborted.
    fn wait_until_ready(&self, should_abort: &dyn Fn() -> bool) -> bool {
        /// Per-iteration wait cap for the processed-state condvar. Caps
        /// the latency of a missed-notification cycle without burning
        /// CPU on tight polling.
        const COND_WAIT_MS: u64 = 100;
        loop {
            if self.is_failed() {
                return false;
            }
            let ready = {
                let guard = self.processed.lock_sync();
                if *guard {
                    return !self.is_failed();
                }
                let deadline = Instant::now() + Duration::from_millis(COND_WAIT_MS);
                let next = self.cv.wait_sync_timeout(guard, deadline);
                *next
            };
            if ready {
                return !self.is_failed();
            }
            if self.is_failed() || should_abort() {
                return false;
            }
        }
    }
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
            readiness: Arc::clone(&self.readiness),
        }
    }
}

impl<R, Ctx> Debug for ProcessedResource<R, Ctx>
where
    R: Debug,
    Ctx: Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedResource")
            .field("inner", &self.inner)
            .field("ctx", &self.ctx)
            .field("is_processed", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: ResourceHandle + Debug,
    Ctx: Clone + Debug,
{
    pub fn new(inner: R, ctx: Option<Ctx>, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        let processed = ctx.is_none() || matches!(inner.status(), ResourceStatus::Committed { .. });
        Self {
            inner,
            ctx,
            process,
            pool,
            readiness: Arc::new(ReadinessGate::new(processed)),
        }
    }
}

impl<R, Ctx> ProcessedResource<R, Ctx>
where
    R: ResourceHandle + Send + Sync + Clone + Debug + 'static,
    Ctx: Clone + Send + Sync + Debug,
{
    /// Whether further waiting is pointless because the inner
    /// resource will never produce processed bytes.
    ///
    /// Both `Failed` and `Cancelled` are terminal here: failure
    /// means a data error, cancellation means the routine shutdown
    /// signal (e.g. track switch, app exit) reached the resource.
    /// Either way the gate will never flip Ready, so a waiter must
    /// abort instead of polling indefinitely.
    fn inner_terminal(&self) -> bool {
        matches!(
            self.inner.status(),
            ResourceStatus::Failed(_) | ResourceStatus::Cancelled
        )
    }

    fn is_readable(&self) -> bool {
        self.ctx.is_none() || self.readiness.is_ready()
    }

    /// Process content chunk-by-chunk and write back to disk.
    ///
    /// Returns the total number of bytes written after processing.
    /// This may be less than `final_len` when the processor removes padding
    /// (e.g. PKCS7 unpadding in AES-128-CBC decryption).
    fn process_and_write(&self, final_len: u64) -> StorageResult<u64> {
        self.ctx.as_ref().map_or(Ok(final_len), |ctx| {
            run_process(&self.inner, ctx, &self.process, &self.pool, final_len)
        })
    }
}

/// Stream `inner`'s raw bytes through `process` chunk-by-chunk and write the
/// transformed bytes back in place. Returns the processed byte count, which may
/// shrink below `final_len` (e.g. PKCS7 unpadding). Shared by the legacy
/// [`ProcessedResource`] and the [`ProcessedWriter`] commit path.
fn run_process<R, Ctx>(
    inner: &R,
    ctx: &Ctx,
    process: &ProcessChunkFn<Ctx>,
    pool: &BytePool,
    final_len: u64,
) -> StorageResult<u64>
where
    R: ResourceHandle,
    Ctx: Clone,
{
    let mut ctx = ctx.clone();

    let mut input_buf = pool.get_with(|b| b.resize(Consts::CHUNK_SIZE, 0));
    let mut output_buf = pool.get_with(|b| b.resize(Consts::CHUNK_SIZE, 0));

    let mut read_offset = 0u64;
    let mut write_offset = 0u64;

    while read_offset < final_len {
        let remaining_u64 = (final_len - read_offset).min(Consts::CHUNK_SIZE_U64);
        let to_read = usize::try_from(remaining_u64).map_err(|err| {
            StorageError::Failed(format!(
                "process_and_write: chunk size {remaining_u64} does not fit usize: {err}"
            ))
        })?;
        let is_last = read_offset + remaining_u64 >= final_len;

        let n = inner.read_at(read_offset, &mut input_buf[..to_read])?;
        if n == 0 {
            break;
        }

        let written = (process)(&input_buf[..n], &mut output_buf[..n], &mut ctx, is_last)
            .map_err(StorageError::Failed)?;

        inner.write_at(write_offset, &output_buf[..written])?;

        read_offset += n as u64;
        write_offset += u64::try_from(written).map_err(|err| {
            StorageError::Failed(format!(
                "process_and_write: written {written} does not fit u64: {err}"
            ))
        })?;
    }

    Ok(write_offset)
}

impl<R, Ctx> ResourceHandle for ProcessedResource<R, Ctx>
where
    R: ResourceHandle + Send + Sync + Clone + Debug + 'static,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let needs_processing = self.ctx.is_some() && !self.readiness.is_ready();
        let actual_len = if needs_processing {
            if let Some(len) = final_len
                && len > 0
            {
                Some(self.process_and_write(len)?)
            } else {
                final_len
            }
        } else {
            final_len
        };

        let inner_result = self.inner.commit(actual_len);
        if inner_result.is_ok() && needs_processing {
            self.readiness.mark_ready();
        }
        inner_result
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.is_readable() && self.inner.contains_range(range)
    }

    fn reactivate(&self) -> StorageResult<()> {
        if self.ctx.is_some() {
            self.readiness.mark_pending();
        }
        self.inner.reactivate()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if !self.is_readable() {
            return Err(StorageError::NotReadable);
        }
        self.inner.read_at(offset, buf)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        let outcome = self.inner.wait_range(range)?;
        if self.ctx.is_none() || outcome != WaitOutcome::Ready {
            return Ok(outcome);
        }
        let aborted = !self.readiness.wait_until_ready(&|| self.inner_terminal());
        if aborted {
            return Ok(WaitOutcome::Interrupted);
        }
        Ok(WaitOutcome::Ready)
    }

    delegate::delegate! {
        to self.inner {
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
            fn fail(&self, reason: String);
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
        }
    }
}

/// Pending (writer) phase of a processed resource — the sole producer handle.
///
/// Owns the write+decrypt capability and a fresh per-generation
/// [`ReadinessGate`]. Has **no read methods**, so reading a not-yet-committed
/// handle is a compile error. [`commit`](Self::commit) consumes the writer into
/// a [`ProcessedReader`]; dropping without `commit`/`fail` fails the gate so a
/// waiting reader cannot deadlock. See the crate `README.md`.
pub struct ProcessedWriter<R, Ctx> {
    readiness: Arc<ReadinessGate>,
    pool: BytePool,
    ctx: Option<Ctx>,
    process: ProcessChunkFn<Ctx>,
    inner: R,
    settled: bool,
}

/// Ready (reader) phase of a processed resource — a cheap-to-clone read view.
///
/// Reads return already-processed bytes. It carries its generation's
/// [`ReadinessGate`]: a view shared with an in-flight writer (via
/// [`ProcessedWriter::reader`]) still blocks in [`wait_range`](Self::wait_range)
/// until that writer commits. [`reactivate`](Self::reactivate) consumes the
/// reader into a fresh [`ProcessedWriter`] with a **new** gate, so reacquiring
/// never poisons other reader clones of the prior generation.
pub struct ProcessedReader<R, Ctx> {
    readiness: Arc<ReadinessGate>,
    pool: BytePool,
    ctx: Option<Ctx>,
    process: ProcessChunkFn<Ctx>,
    inner: R,
}

impl<R, Ctx> Clone for ProcessedReader<R, Ctx>
where
    R: Clone,
    Ctx: Clone,
{
    fn clone(&self) -> Self {
        Self {
            readiness: Arc::clone(&self.readiness),
            pool: self.pool.clone(),
            ctx: self.ctx.clone(),
            process: Arc::clone(&self.process),
            inner: self.inner.clone(),
        }
    }
}

impl<R: Debug, Ctx: Debug> Debug for ProcessedWriter<R, Ctx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedWriter")
            .field("inner", &self.inner)
            .field("ctx", &self.ctx)
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

impl<R: Debug, Ctx: Debug> Debug for ProcessedReader<R, Ctx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedReader")
            .field("inner", &self.inner)
            .field("ctx", &self.ctx)
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

impl<R, Ctx> Drop for ProcessedWriter<R, Ctx> {
    fn drop(&mut self) {
        if !self.settled {
            self.readiness.fail();
        }
    }
}

impl<R, Ctx> ProcessedWriter<R, Ctx>
where
    R: ResourceHandle + Clone + Debug,
    Ctx: Clone + Debug,
{
    /// Create a fresh pending writer. The gate starts ready only when no
    /// processing is required (`ctx` is `None`); an encrypted writer is pending
    /// until [`commit`](WriteSide::commit).
    pub fn new(inner: R, ctx: Option<Ctx>, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        let ready = ctx.is_none();
        Self {
            inner,
            ctx,
            process,
            pool,
            readiness: Arc::new(ReadinessGate::new(ready)),
            settled: false,
        }
    }

    /// A read view sharing this writer's generation gate; reads block in
    /// [`wait_range`](ReadSide::wait_range) until this writer commits. The view
    /// is `Clone`, the writer is not.
    #[must_use]
    pub fn reader(&self) -> ProcessedReader<R, Ctx> {
        ProcessedReader {
            readiness: Arc::clone(&self.readiness),
            pool: self.pool.clone(),
            ctx: self.ctx.clone(),
            process: Arc::clone(&self.process),
            inner: self.inner.clone(),
        }
    }
}

impl<R, Ctx> ProcessedReader<R, Ctx>
where
    R: ResourceHandle + Clone + Debug,
    Ctx: Clone + Debug,
{
    fn is_readable(&self) -> bool {
        self.ctx.is_none() || self.readiness.is_ready()
    }

    /// A waiter must abort when the gate failed (writer dropped) or the backing
    /// resource reached a terminal state — neither will ever flip ready.
    fn inner_terminal(&self) -> bool {
        self.readiness.is_failed()
            || matches!(
                self.inner.status(),
                ResourceStatus::Failed(_) | ResourceStatus::Cancelled
            )
    }

    /// Consume the reader and reopen for writing with a **fresh** gate, so a
    /// concurrent reader clone of the prior generation is never poisoned.
    ///
    /// # Errors
    /// Propagates the backing reactivate error.
    pub fn reactivate(self) -> StorageResult<ProcessedWriter<R, Ctx>> {
        self.inner.reactivate()?;
        let ready = self.ctx.is_none();
        Ok(ProcessedWriter {
            readiness: Arc::new(ReadinessGate::new(ready)),
            pool: self.pool,
            ctx: self.ctx,
            process: self.process,
            inner: self.inner,
            settled: false,
        })
    }
}

impl<R, Ctx> WriteSide for ProcessedWriter<R, Ctx>
where
    R: ResourceHandle + Clone + Send + Sync + Debug + 'static,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    type Reader = ProcessedReader<R, Ctx>;

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn commit(mut self, final_len: Option<u64>) -> StorageResult<ProcessedReader<R, Ctx>> {
        let needs_processing = self.ctx.is_some() && !self.readiness.is_ready();
        let actual_len = match (needs_processing, final_len) {
            (true, Some(len)) if len > 0 => match &self.ctx {
                Some(ctx) => Some(run_process(
                    &self.inner,
                    ctx,
                    &self.process,
                    &self.pool,
                    len,
                )?),
                None => final_len,
            },
            _ => final_len,
        };

        self.inner.commit(actual_len)?;
        if needs_processing {
            self.readiness.mark_ready();
        }
        self.settled = true;
        Ok(self.reader())
    }

    fn fail(mut self, reason: String) {
        self.inner.fail(reason);
        self.readiness.fail();
        self.settled = true;
    }
}

impl<R, Ctx> ReadSide for ProcessedReader<R, Ctx>
where
    R: ResourceHandle + Clone + Send + Sync + Debug + 'static,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if !self.is_readable() {
            return Err(StorageError::NotReadable);
        }
        self.inner.read_at(offset, buf)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        let outcome = self.inner.wait_range(range)?;
        if self.ctx.is_none() || outcome != WaitOutcome::Ready {
            return Ok(outcome);
        }
        if self.readiness.wait_until_ready(&|| self.inner_terminal()) {
            Ok(WaitOutcome::Ready)
        } else {
            Ok(WaitOutcome::Interrupted)
        }
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.is_readable() && self.inner.contains_range(range)
    }

    fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.inner.next_gap(from, limit)
    }

    fn path(&self) -> Option<&Path> {
        self.inner.path()
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
    pool: BytePool,
    process: ProcessChunkFn<Ctx>,
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
            pool,
            process,
        }
    }

    #[must_use]
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
    type Context = Ctx;
    type IndexRes = A::IndexRes;
    type Res = ProcessedResource<A::Res, Ctx>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        Ok(self.wrap(self.inner.acquire_resource(key, identity)?, ctx))
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        Ok(self.wrap(self.inner.open_resource(key, identity)?, ctx))
    }

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> crate::base::Capabilities;
            fn root_dir(&self) -> &Path;
            fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;
            fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;
            fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
        }
    }
}

impl<A, Ctx> ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    fn wrap(&self, inner: A::Res, ctx: Option<Ctx>) -> ProcessedResource<A::Res, Ctx> {
        ProcessedResource::new(inner, ctx, Arc::clone(&self.process), self.pool.clone())
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use kithara_storage::{MmapOptions, MmapResource, Resource, StorageResource};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::AssetStoreBuilder;

    fn test_pool() -> BytePool {
        BytePool::new(4, Consts::CHUNK_SIZE)
    }

    /// Simple mock resource for testing.
    /// Returns both the resource and the `TempDir` to keep the directory alive.
    fn mock_resource(content: &[u8]) -> (StorageResource, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let cancel = CancellationToken::new();

        let res: MmapResource = Resource::open(cancel, MmapOptions::new(path)).unwrap();
        res.write_at(0, content).unwrap();
        (StorageResource::from(res), dir)
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

    #[kithara::test]
    fn test_process_on_commit() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(b"test content");
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        assert_eq!(call_count.load(Ordering::SeqCst), 0);

        processed
            .commit(Some(b"test content".len() as u64))
            .unwrap();
        assert!(call_count.load(Ordering::SeqCst) > 0);

        let mut buf = vec![0u8; 12];
        let n = processed.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);

        let expected: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(buf, expected);
    }

    #[kithara::test]
    #[case::short(&b"data"[..], 4)]
    #[case::longer(&b"abcdef"[..], 6)]
    fn test_process_called_once_on_multiple_commits(#[case] content: &[u8], #[case] len: u64) {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(content);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        processed.commit(Some(len)).unwrap();
        let count_after_first = call_count.load(Ordering::SeqCst);
        assert!(count_after_first > 0);

        processed.commit(Some(len)).unwrap();
        assert_eq!(call_count.load(Ordering::SeqCst), count_after_first);
    }

    #[kithara::test]
    fn test_read_at_after_processing() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0xFF, Arc::clone(&call_count));

        let content: Vec<u8> = (0..100).collect();
        let (resource, _dir) = mock_resource(&content);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        processed.commit(Some(100)).unwrap();

        let mut buf = vec![0u8; 20];
        let n = processed.read_at(40, &mut buf).unwrap();
        assert_eq!(n, 20);

        let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
        assert_eq!(buf, expected);
    }

    #[kithara::test]
    fn test_no_processing_without_ctx() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(b"test content");
        let processed: ProcessedResource<StorageResource, ()> =
            ProcessedResource::new(resource, None, process_fn, test_pool());

        processed
            .commit(Some(b"test content".len() as u64))
            .unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 0);

        let mut buf = vec![0u8; 12];
        let n = processed.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"test content");
    }

    #[kithara::test]
    fn encrypted_resource_is_not_readable_before_commit() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(b"test content");
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        assert!(
            !processed.contains_range(0..12),
            "encrypted active resource must not advertise readable ranges before processing"
        );

        let mut buf = vec![0u8; 12];
        let err = processed
            .read_at(0, &mut buf)
            .expect_err("encrypted active resource must reject reads before commit");
        assert!(
            err.to_string().contains("not readable before commit"),
            "read guard must explain that commit is required before reads"
        );
    }

    #[kithara::test]
    fn reopened_committed_processed_resource_is_readable_immediately() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let already_processed: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();

        let (resource, _dir) = mock_resource(&already_processed);
        resource
            .commit(Some(already_processed.len() as u64))
            .unwrap();

        let reopened = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        let mut buf = vec![0u8; already_processed.len()];
        let n = reopened.read_at(0, &mut buf).unwrap();

        assert_eq!(n, already_processed.len());
        assert_eq!(buf, already_processed);
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
    }

    /// Cache fall-through: `open_resource(key)` (ctx=None) must not return
    /// an uncommitted DRM-style entry that a concurrent
    /// `acquire_resource_with_ctx(key, Some(ctx))` parked in the cache.
    ///
    /// Reading such an entry trips the guard
    /// `"processed resource is not readable before commit"` (see
    /// [`ProcessedResource::read_at`]).
    #[kithara::test]
    fn red_test_fixed_seek_window_open_resource_returns_uncommitted_processed_entry() {
        #[derive(Clone, Debug, Hash, Eq, PartialEq, Default)]
        struct DrmCtx {
            xor_key: u8,
        }

        let process_fn: ProcessChunkFn<DrmCtx> = Arc::new(
            |input: &[u8], output: &mut [u8], ctx: &mut DrmCtx, _is_last: bool| {
                for (i, &b) in input.iter().enumerate() {
                    output[i] = b ^ ctx.xor_key;
                }
                Ok(input.len())
            },
        );

        let store = AssetStoreBuilder::new()
            .process_fn(process_fn)
            .ephemeral(true)
            .build();
        let key = ResourceKey::relative("drm-fallthrough", "segment.m4s");
        let ctx = DrmCtx { xor_key: 0x42 };

        let writer = store
            .acquire_resource_with_ctx(&key, None, Some(ctx.clone()))
            .expect("BUG: acquire with ctx must succeed");
        let payload = b"uncommitted encrypted bytes";
        writer
            .write_at(0, payload)
            .expect("BUG: writer must be able to stream bytes before commit");

        match store.open_resource(&key, None) {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    !msg.contains("processed resource is not readable before commit"),
                    "open_resource (ctx=None) leaked the pre-commit guard: {msg}"
                );
            }
            Ok(reader) => {
                let mut buf = vec![0u8; payload.len()];
                if let Err(err) = reader.read_at(0, &mut buf) {
                    let msg = err.to_string();
                    assert!(
                        !msg.contains("processed resource is not readable before commit"),
                        "read_at(ctx=None) leaked the pre-commit guard from a \
                         concurrent ctx=Some writer: {msg}"
                    );
                }
            }
        }
    }

    /// RED test (integration: `live_ephemeral_small_cache_playback_drm`)
    ///
    /// Scenario: an ephemeral DRM stream evicts and then re-acquires the
    /// same `(ResourceKey, Some(ctx))` slot while the entry is still in
    /// the LRU cache (e.g. after `reactivate()` on cache hit). The
    /// downloader streams fresh *encrypted* bytes, then commits.
    ///
    /// Invariant under test: `ProcessedResource::commit` must decrypt the
    /// newly written bytes even when the resource was previously
    /// committed once and then reactivated. Today the `processed` flag
    /// persists across `reactivate()`, so the second commit skips
    /// `process_and_write` — the "decrypted" bytes the reader sees are
    /// the raw *encrypted* payload, and the audio decoder either stalls
    /// on a broken stream or returns silence.
    ///
    /// Captures a DRM-specific contract (playback path exercised by the
    /// `::drm` case of `live_ephemeral_small_cache_playback`).
    #[kithara::test]
    fn red_test_drm_small_cache_reactivate_preserves_processed_flag() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let encrypted_first: Vec<u8> = b"first  payload".iter().map(|b| b ^ 0x42).collect();
        let encrypted_second: Vec<u8> = b"second payload".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(
            encrypted_first.len(),
            encrypted_second.len(),
            "test payloads must have equal length",
        );

        let (resource, _dir) = mock_resource(&encrypted_first);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        let len = encrypted_first.len() as u64;
        processed.commit(Some(len)).expect("BUG: first commit");
        let first_call_count = call_count.load(Ordering::SeqCst);
        assert!(
            first_call_count > 0,
            "first commit must invoke the DRM processor"
        );
        assert!(matches!(
            processed.status(),
            ResourceStatus::Committed { .. }
        ));

        processed
            .reactivate()
            .expect("BUG: reactivate after commit must succeed");
        assert!(
            matches!(processed.status(), ResourceStatus::Active),
            "reactivate must clear committed state"
        );

        processed
            .write_at(0, &encrypted_second)
            .expect("BUG: re-write encrypted bytes");
        processed.commit(Some(len)).expect("BUG: second commit");

        let second_call_count = call_count.load(Ordering::SeqCst);
        assert!(
            second_call_count > first_call_count,
            "second commit must rerun the DRM processor (processed flag \
             was retained across reactivate — decryption is skipped and \
             the reader observes raw ciphertext, matching the DRM \
             small-cache stall)"
        );

        let mut out = vec![0u8; encrypted_second.len()];
        processed
            .read_at(0, &mut out)
            .expect("BUG: read_at after second commit");
        assert_eq!(
            &out[..],
            b"second payload",
            "reader must see plaintext after the reactivated resource is \
             re-committed; if bytes are still ciphertext the DRM \
             processor was skipped"
        );
    }

    /// Guards the DRM small-cache flake (`live_ephemeral_small_cache_playback_drm`):
    /// after `acquire_resource_with_ctx(K, Some)` reactivates a just-committed
    /// cache entry (flipping `processed` to `false`), a concurrent reader holding
    /// a cloned `Arc` must not see `contains_range = true` yet hit the pre-commit
    /// read guard. The availability view and `read_at` must agree.
    #[kithara::test]
    fn red_test_drm_small_cache_writer_reactivate_poisons_concurrent_reader() {
        #[derive(Clone, Debug, Hash, Eq, PartialEq, Default)]
        struct DrmCtx {
            xor_key: u8,
        }

        let process_fn: ProcessChunkFn<DrmCtx> = Arc::new(
            |input: &[u8], output: &mut [u8], ctx: &mut DrmCtx, _is_last: bool| {
                for (i, &b) in input.iter().enumerate() {
                    output[i] = b ^ ctx.xor_key;
                }
                Ok(input.len())
            },
        );

        let store = AssetStoreBuilder::new()
            .process_fn(process_fn)
            .ephemeral(true)
            .cache_capacity(NonZeroUsize::new(4).expect("BUG: nonzero"))
            .build();
        let key = ResourceKey::relative("drm-reactivate-poisons-reader", "segment-0.m4s");
        let ctx = DrmCtx { xor_key: 0x42 };
        let plaintext = b"hello drm world";
        let ciphertext: Vec<u8> = plaintext.iter().map(|b| b ^ 0x42).collect();

        {
            let a = store
                .acquire_resource_with_ctx(&key, None, Some(ctx.clone()))
                .expect("BUG: writer A acquire");
            a.write_at(0, &ciphertext).expect("BUG: writer A write");
            a.commit(Some(ciphertext.len() as u64))
                .expect("BUG: writer A commit");
        }

        assert!(
            store.contains_range(&key, 0..ciphertext.len() as u64),
            "availability must advertise the committed range before \
             the writer B reactivate"
        );

        let reader = store
            .open_resource(&key, None)
            .expect("BUG: reader open_resource after commit");

        let mut probe = vec![0u8; ciphertext.len()];
        let n = reader
            .read_at(0, &mut probe)
            .expect("BUG: reader sanity read before writer B");
        assert_eq!(n, ciphertext.len());
        assert_eq!(
            &probe[..],
            plaintext,
            "reader must see decrypted plaintext before any \
             reactivate race"
        );

        let _writer_b = store
            .acquire_resource_with_ctx(&key, None, Some(ctx.clone()))
            .expect("BUG: writer B reacquire");

        assert!(
            store.contains_range(&key, 0..ciphertext.len() as u64),
            "availability must still advertise the range after a \
             cache-hit reactivate (no LRU displace occurred)"
        );

        let mut buf = vec![0u8; ciphertext.len()];
        match reader.read_at(0, &mut buf) {
            Ok(n) => {
                assert_eq!(
                    &buf[..n],
                    plaintext,
                    "reader clone must keep seeing the committed \
                     plaintext across a concurrent writer-side \
                     reactivate; got {:?}",
                    &buf[..n]
                );
            }
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    !msg.contains("not readable before commit"),
                    "writer B's reactivate poisoned a concurrent \
                     reader holding a cloned Arc to the same \
                     ProcessedResource — reader's read_at fires the \
                     pre-commit guard: {msg}. This is the \
                     DRM-specific structural race behind \
                     live_ephemeral_small_cache_playback_drm: \
                     availability still advertises the committed \
                     range (no on_invalidated fired), so the HLS \
                     reader passes its contains_range gate and then \
                     hits this guard as a hard StorageError::Failed, \
                     which propagates as StreamError::Source and \
                     poisons the decoder FSM."
                );
            }
        }
    }

    /// Concurrent contract pinning the post-commit readiness gate.
    ///
    /// Reader spawns first, parks in `wait_range` while bytes-on-disk
    /// are already `Ready` but processing has not yet run. The writer
    /// thread fires `commit()` after a short delay; `wait_range` must
    /// wake only after `process_and_write` has finished and `read_at`
    /// must return the *processed* bytes (not the raw on-disk bytes).
    ///
    /// Before the gate fix this race collapsed to `wait_range = Ready`
    /// + `read_at = NotReadable`, the production hang surfaced by
    /// `local_queue_playlist_behavior_symphonia`.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn wait_range_blocks_until_commit_processes() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x55, Arc::clone(&call_count));
        let raw: Vec<u8> = (0..32u8).collect();

        let (resource, _dir) = mock_resource(&raw);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());
        let processed_for_writer = processed.clone();
        let raw_len = raw.len() as u64;

        let reader = std::thread::spawn(move || {
            let outcome = processed.wait_range(0..raw_len).unwrap();
            assert_eq!(outcome, WaitOutcome::Ready);
            assert!(
                processed.is_readable(),
                "wait_range must not return Ready before processing has run"
            );
            let mut buf = vec![0u8; raw.len()];
            processed.read_at(0, &mut buf).unwrap();
            buf
        });

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "process must not have run before commit"
        );
        processed_for_writer.commit(Some(raw_len)).unwrap();

        let read = reader.join().unwrap();
        let expected: Vec<u8> = (0..32u8).map(|b| b ^ 0x55).collect();
        assert_eq!(
            read, expected,
            "reader must observe the processed (XOR'd) bytes, not raw"
        );
    }

    /// Cancellation contract: a reader parked in the readiness gate
    /// must wake when the underlying resource's cancel token fires —
    /// not poll on the watchdog tick until something else nudges it.
    /// Surfacing the cancel through `ResourceStatus::Cancelled` is
    /// what closes that observation gap.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn wait_range_aborts_on_cancellation() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

        let dir = tempdir().unwrap();
        let path = dir.path().join("cancel.bin");
        let cancel = CancellationToken::new();
        let resource: MmapResource =
            Resource::open(cancel.clone(), MmapOptions::new(path)).unwrap();
        resource.write_at(0, &[1u8; 16]).unwrap();

        let processed = ProcessedResource::new(
            StorageResource::from(resource),
            Some(()),
            process_fn,
            test_pool(),
        );
        let processed_for_reader = processed.clone();

        let reader = std::thread::spawn(move || processed_for_reader.wait_range(0..16));

        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();

        let outcome = reader
            .join()
            .expect("BUG: reader thread panicked")
            .expect("BUG: wait_range must not surface a hard error on cancel");
        assert_eq!(
            outcome,
            WaitOutcome::Interrupted,
            "cancellation must wake the gate as Interrupted, not block on the 100ms tick"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "processor must not have run after cancellation"
        );
    }

    #[kithara::test]
    fn writer_commit_returns_readable_reader() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let (resource, _dir) = mock_resource(b"test content");

        let writer = ProcessedWriter::new(resource, Some(()), process_fn, test_pool());
        let reader = writer.commit(Some(b"test content".len() as u64)).unwrap();
        assert!(call_count.load(Ordering::SeqCst) > 0);

        let mut buf = vec![0u8; 12];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        let expected: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(buf, expected);
    }

    #[kithara::test]
    fn ctx_none_writer_commits_straight_through_to_readable() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let (resource, _dir) = mock_resource(b"plain bytes!");

        let writer: ProcessedWriter<StorageResource, ()> =
            ProcessedWriter::new(resource, None, process_fn, test_pool());
        let reader = writer.commit(Some(12)).unwrap();

        let mut buf = vec![0u8; 12];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        assert_eq!(&buf, b"plain bytes!", "ctx=None reads raw, unprocessed");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "no processor runs for ctx=None"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reader_view_blocks_until_writer_commits() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x55, Arc::clone(&call_count));
        let raw: Vec<u8> = (0..32u8).collect();
        let (resource, _dir) = mock_resource(&raw);

        let writer = ProcessedWriter::new(resource, Some(()), process_fn, test_pool());
        let reader = writer.reader();
        let raw_len = raw.len() as u64;

        let handle = std::thread::spawn(move || {
            let outcome = reader.wait_range(0..raw_len).unwrap();
            assert_eq!(outcome, WaitOutcome::Ready);
            let mut buf = vec![0u8; 32];
            reader.read_at(0, &mut buf).unwrap();
            buf
        });

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "process must not run before commit"
        );
        let _committed = writer.commit(Some(raw_len)).unwrap();

        let read = handle.join().unwrap();
        let expected: Vec<u8> = (0..32u8).map(|b| b ^ 0x55).collect();
        assert_eq!(read, expected, "reader view must observe processed bytes");
    }

    #[kithara::test]
    fn reactivate_forks_fresh_gate_without_poisoning_reader() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let plaintext = b"hello drm world";
        let ciphertext: Vec<u8> = plaintext.iter().map(|b| b ^ 0x42).collect();
        let (resource, _dir) = mock_resource(&ciphertext);

        let writer = ProcessedWriter::new(resource, Some(()), process_fn, test_pool());
        let reader_a = writer.commit(Some(ciphertext.len() as u64)).unwrap();

        let mut buf = vec![0u8; ciphertext.len()];
        reader_a.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..], plaintext, "committed reader sees plaintext");

        let reader_b = reader_a.clone();
        let _writer_b = reader_a
            .reactivate()
            .expect("reactivate mints a fresh-gate writer");

        let mut buf2 = vec![0u8; ciphertext.len()];
        let n = reader_b
            .read_at(0, &mut buf2)
            .expect("reader clone must keep reading committed plaintext across a reactivate");
        assert_eq!(
            &buf2[..n],
            plaintext,
            "fresh-gate reactivate must not poison a prior-generation reader clone \
             (the structural root of live_ephemeral_small_cache_playback_drm)"
        );
    }

    #[kithara::test]
    fn reactivate_then_commit_reruns_processor() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let first: Vec<u8> = b"first  payload".iter().map(|b| b ^ 0x42).collect();
        let second: Vec<u8> = b"second payload".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(first.len(), second.len());
        let (resource, _dir) = mock_resource(&first);

        let writer = ProcessedWriter::new(resource, Some(()), process_fn, test_pool());
        let len = first.len() as u64;
        let reader = writer.commit(Some(len)).expect("first commit");
        let first_count = call_count.load(Ordering::SeqCst);
        assert!(first_count > 0, "first commit runs the processor");

        let writer2 = reader.reactivate().expect("reactivate after commit");
        writer2.write_at(0, &second).expect("re-write ciphertext");
        let reader2 = writer2.commit(Some(len)).expect("second commit");
        assert!(
            call_count.load(Ordering::SeqCst) > first_count,
            "fresh gate -> second commit reruns the processor (decrypt not skipped)"
        );

        let mut out = vec![0u8; second.len()];
        reader2
            .read_at(0, &mut out)
            .expect("read after second commit");
        assert_eq!(&out[..], b"second payload");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn writer_drop_without_commit_fails_gate() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));
        let (resource, _dir) = mock_resource(&[7u8; 16]);

        let writer = ProcessedWriter::new(resource, Some(()), process_fn, test_pool());
        writer.write_at(0, &[7u8; 16]).unwrap();
        let reader = writer.reader();
        let reader_probe = reader.clone();

        let handle = std::thread::spawn(move || reader.wait_range(0..16));
        std::thread::sleep(Duration::from_millis(50));
        drop(writer);

        let outcome = handle
            .join()
            .expect("BUG: reader thread panicked")
            .expect("BUG: wait_range must not surface a hard error on gate fail");
        assert_eq!(
            outcome,
            WaitOutcome::Interrupted,
            "dropping a writer without commit must wake a parked reader, not deadlock"
        );

        let mut buf = [0u8; 16];
        assert!(
            matches!(
                reader_probe.read_at(0, &mut buf),
                Err(StorageError::NotReadable)
            ),
            "a failed (never-committed) encrypted resource must reject read_at, \
             not return raw/partial bytes"
        );
    }
}
