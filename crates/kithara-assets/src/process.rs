#![forbid(unsafe_code)]

//! Processing layer for resources.
//!
//! Processes content chunk-by-chunk on commit, writes to disk.
//! Uses buffer pool — no allocations during processing.

use std::{fmt, fmt::Debug, hash::Hash, ops::Range, path::Path, sync::Arc};

use kithara_bufpool::BytePool;
use kithara_platform::{Condvar, Mutex, time::Instant};
use kithara_storage::{ResourceExt, ResourceStatus, StorageError, StorageResult, WaitOutcome};

use crate::{AssetResourceState, AssetsResult, ResourceKey, base::Assets};

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
///
/// ## Readability sync
///
/// `inner.wait_range` reports `Ready` as soon as bytes hit disk via
/// `write_at`, but for an active processor those bytes are still
/// **encrypted** until [`Self::commit`] runs `process_and_write`. A
/// reader that observed `wait_range = Ready` and immediately called
/// `read_at` would race the processor and hit
/// [`StorageError::NotReadable`] (the symptom that surfaced as the
/// `local_queue_playlist_behavior_*` post-seek hang under DRM).
///
/// `ProcessedResource` therefore owns a [`ReadinessGate`] that pairs
/// the `processed` flag with a [`Condvar`]: `commit()` flips the flag
/// and notifies, and `wait_range`/`read_at` block on the gate so
/// callers cannot see "ready bytes" until processing has finished.
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
}

impl ReadinessGate {
    fn new(initial: bool) -> Self {
        Self {
            processed: Mutex::new(initial),
            cv: Condvar::new(),
        }
    }

    fn is_ready(&self) -> bool {
        *self.processed.lock_sync()
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
        loop {
            // Bounded wake-up so a missed notify (or a status change
            // that does not flow through `mark_ready`) cannot strand
            // a reader. 100 ms keeps perceived latency under the
            // 250 ms playback engine tick budget. The result is read
            // out of the guard, which is dropped at the end of the
            // block so `should_abort()` (which may take the inner
            // resource's locks) cannot deadlock against a producer
            // that wants the gate's own mutex.
            let ready = {
                let guard = self.processed.lock_sync();
                if *guard {
                    return true;
                }
                let deadline = Instant::now() + std::time::Duration::from_millis(100);
                let (next, _) = self.cv.wait_sync_timeout(guard, deadline);
                *next
            };
            if ready {
                return true;
            }
            if should_abort() {
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
    R: ResourceExt + Debug,
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
    R: ResourceExt + Send + Sync + Clone + Debug + 'static,
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
            #[expect(
                clippy::cast_possible_truncation,
                reason = "remaining is bounded by chunk_size (64KB) via min() on next line"
            )]
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
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        // Process on commit (once) if ctx is present.
        // Use the actual processed length (may differ due to padding removal).
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
        // Mark readiness only after the inner commit succeeds — a
        // failed inner commit must not unblock waiters that would
        // then race against a half-written file.
        if inner_result.is_ok() && needs_processing {
            self.readiness.mark_ready();
        }
        inner_result
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.is_readable() && self.inner.contains_range(range)
    }

    fn reactivate(&self) -> StorageResult<()> {
        // Reactivation reopens the inner resource for fresh writes (LRU
        // slot reuse). Flip the gate to Pending **before** touching
        // `inner.reactivate` so `is_readable()` cannot lie during the
        // window where the inner has been reopened but the next commit
        // has not yet rerun the processor — otherwise a concurrent
        // reader sees `processed = true` while the inner is mid-truncate
        // and reads half-overwritten bytes. The flag is restored to
        // Ready by the next successful commit.
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
        // No active processor → bytes-on-disk readiness *is* readiness.
        // Forward the inner outcome unchanged. Same for `Eof` /
        // `Interrupted`: those terminal/control variants do not depend
        // on processing.
        if self.ctx.is_none() || outcome != WaitOutcome::Ready {
            return Ok(outcome);
        }
        // Active processor: inner reports Ready as soon as bytes hit
        // disk via write_at, but those bytes are still encrypted
        // until commit() runs process_and_write. Block on the
        // readiness gate so the caller cannot race past processing
        // into a NotReadable read.
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
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        Ok(self.wrap(self.inner.acquire_resource(key)?, ctx))
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        Ok(self.wrap(self.inner.open_resource(key)?, ctx))
    }

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> crate::base::Capabilities;
            fn root_dir(&self) -> &Path;
            fn asset_root(&self) -> &str;
            fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;
            fn delete_asset(&self) -> AssetsResult<()>;
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

    use kithara_storage::{MmapOptions, MmapResource, Resource};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::AssetStoreBuilder;

    fn test_pool() -> BytePool {
        BytePool::new(4, PROCESS_CHUNK_SIZE)
    }

    /// Simple mock resource for testing.
    /// Returns both the resource and the `TempDir` to keep the directory alive.
    fn mock_resource(content: &[u8]) -> (MmapResource, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let cancel = CancellationToken::new();

        let res = Resource::open(cancel, MmapOptions::new(path)).unwrap();
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

    #[kithara::test]
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

    #[kithara::test]
    #[case::short(&b"data"[..], 4)]
    #[case::longer(&b"abcdef"[..], 6)]
    fn test_process_called_once_on_multiple_commits(#[case] content: &[u8], #[case] len: u64) {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

        let (resource, _dir) = mock_resource(content);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        // First commit
        processed.commit(Some(len)).unwrap();
        let count_after_first = call_count.load(Ordering::SeqCst);
        assert!(count_after_first > 0);

        // Second commit - should not process again
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

        // Read middle portion
        let mut buf = vec![0u8; 20];
        let n = processed.read_at(40, &mut buf).unwrap();
        assert_eq!(n, 20);

        // Verify XOR
        let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
        assert_eq!(buf, expected);
    }

    #[kithara::test]
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
            .asset_root(Some("drm-fallthrough"))
            .process_fn(process_fn)
            .ephemeral(true)
            .build();

        let key = ResourceKey::new("segment.m4s");
        let ctx = DrmCtx { xor_key: 0x42 };

        // Writer acquires with ctx and streams some bytes but does NOT commit.
        // This parks a ProcessedResource with ctx=Some, processed=false in the cache.
        let writer = store
            .acquire_resource_with_ctx(&key, Some(ctx.clone()))
            .expect("acquire with ctx must succeed");
        let payload = b"uncommitted encrypted bytes";
        writer
            .write_at(0, payload)
            .expect("writer must be able to stream bytes before commit");

        // Reader asks for the same key without ctx.  Under the old
        // behaviour, the cache's ctx=None fall-through returned the
        // writer's uncommitted ProcessedResource entry, so a subsequent
        // read surfaced the "processed resource is not readable before
        // commit" guard as a hard error. Acceptable correct behaviours:
        //   1) `open_resource` fails cleanly (NotFound / recoverable Err),
        //      which the reader converts to `ReadOutcome::Retry`.
        //   2) `open_resource` succeeds and the read does NOT expose the
        //      pre-commit guard.
        match store.open_resource(&key) {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    !msg.contains("processed resource is not readable before commit"),
                    "open_resource (ctx=None) leaked the pre-commit guard: {msg}"
                );
                // Clean NotFound / recoverable Err is acceptable.
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

        // First wave: download + commit — process_and_write decrypts.
        let (resource, _dir) = mock_resource(&encrypted_first);
        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());

        let len = encrypted_first.len() as u64;
        processed.commit(Some(len)).expect("first commit");
        let first_call_count = call_count.load(Ordering::SeqCst);
        assert!(
            first_call_count > 0,
            "first commit must invoke the DRM processor"
        );
        assert!(matches!(
            processed.status(),
            ResourceStatus::Committed { .. }
        ));

        // Simulate LRU cache re-use: `CachedAssets` calls `reactivate()`
        // on a cache hit so the resource becomes writable again.
        processed
            .reactivate()
            .expect("reactivate after commit must succeed");
        assert!(
            matches!(processed.status(), ResourceStatus::Active),
            "reactivate must clear committed state"
        );

        // Second wave: downloader writes fresh encrypted bytes over the
        // reactivated slot and commits again. commit() must rerun
        // process_and_write — otherwise the reader ends up staring at
        // ciphertext and the decoder stalls, which is exactly the
        // symptom in `live_ephemeral_small_cache_playback_drm`.
        processed
            .write_at(0, &encrypted_second)
            .expect("re-write encrypted bytes");
        processed.commit(Some(len)).expect("second commit");

        let second_call_count = call_count.load(Ordering::SeqCst);
        assert!(
            second_call_count > first_call_count,
            "second commit must rerun the DRM processor (processed flag \
             was retained across reactivate — decryption is skipped and \
             the reader observes raw ciphertext, matching the DRM \
             small-cache stall)"
        );

        // And the on-disk bytes must be the decrypted plaintext — the
        // reader's path in `HlsSource::read_from_entry` expects clear
        // bytes after commit.
        let mut out = vec![0u8; encrypted_second.len()];
        processed
            .read_at(0, &mut out)
            .expect("read_at after second commit");
        assert_eq!(
            &out[..],
            b"second payload",
            "reader must see plaintext after the reactivated resource is \
             re-committed; if bytes are still ciphertext the DRM \
             processor was skipped"
        );
    }

    /// RED test (integration: `live_ephemeral_small_cache_playback_drm`)
    ///
    /// Root cause hypothesis for the DRM-only flake:
    ///
    /// The availability index and the LRU-cached `ProcessedResource`
    /// disagree when a writer calls `acquire_resource_with_ctx(K, Some)`
    /// on an entry that was *just committed* by another writer. The
    /// cache hit triggers `ProcessedResource::reactivate()`, which flips
    /// `processed` to `false`. The shared `AvailabilityIndex` still
    /// advertises the committed range (nobody removed it — this was not
    /// an LRU displace, so `on_invalidated` never ran). A concurrent
    /// reader that holds a cloned `Arc` to the same `ProcessedResource`
    /// then:
    ///
    ///   1. sees `contains_range` → true (availability still says
    ///      committed),
    ///   2. calls `read_at` → fires the pre-commit guard
    ///      ("processed resource is not readable before commit").
    ///
    /// In `HlsSource::read_from_entry` this is propagated as
    /// `Err(StreamError::Source(..))`, which poisons the decoder FSM
    /// (`TrackState::Failed`) and makes `next_chunk_with_timeout`
    /// panic at `stage='ephemeral_small_cache'`.
    ///
    /// Sibling case (`_hls`) is unaffected because without DRM context
    /// `ProcessedResource::is_readable()` short-circuits on
    /// `ctx.is_none()`, and `reactivate()` never poisons reads.
    ///
    /// Contract under test: after
    /// `acquire_resource_with_ctx(K, Some(ctx))` observes an existing
    /// *committed* entry in cache, the `contains_range` view over that
    /// entry and the `read_at` path must agree. Either the availability
    /// index clears the range on reactivate (preferred), or reactivate
    /// avoids flipping `processed` while an uncommitted write has not
    /// arrived, or the reader gets a committed snapshot that is unaffected
    /// by subsequent writer reactivations.
    ///
    /// Deterministic construction:
    ///   1. Build an ephemeral `AssetStore` (same shape as the failing
    ///      integration test).
    ///   2. Writer A acquires `(K, Some(ctx))`, writes + commits →
    ///      availability records the commit, cache holds the entry.
    ///   3. Reader asks `open_resource(K)` (ctx=None) → cache returns
    ///      the committed entry via the ctx=None fall-through. `Reader`
    ///      holds a cloned `Arc` to the same `ProcessedResource`.
    ///   4. `contains_range(K, 0..N)` returns `true` — the reader's
    ///      gate has passed.
    ///   5. Writer B calls `acquire_resource_with_ctx(K, Some(ctx))` →
    ///      cache hit → `res.reactivate()` → `processed = false`.
    ///   6. Reader proceeds with `read_at(0, &mut buf)`.
    ///
    /// Expected: either the reader still sees the committed plaintext
    /// (preferred), or the read returns a benign `NotFound`-style error
    /// so `HlsSource` converts to `ReadOutcome::Retry`. Today it returns
    /// `StorageError::Failed("processed resource is not readable before
    /// commit")`, which is a hard error.
    #[kithara::test]
    fn red_test_drm_small_cache_writer_reactivate_poisons_concurrent_reader() {
        #[derive(Clone, Debug, Hash, Eq, PartialEq, Default)]
        struct DrmCtx {
            xor_key: u8,
        }

        // AES-128-CBC is modelled as XOR here — the bug is about the
        // processed flag bookkeeping, not about cipher correctness.
        let process_fn: ProcessChunkFn<DrmCtx> = Arc::new(
            |input: &[u8], output: &mut [u8], ctx: &mut DrmCtx, _is_last: bool| {
                for (i, &b) in input.iter().enumerate() {
                    output[i] = b ^ ctx.xor_key;
                }
                Ok(input.len())
            },
        );

        let store = AssetStoreBuilder::new()
            .asset_root(Some("drm-reactivate-poisons-reader"))
            .process_fn(process_fn)
            .ephemeral(true)
            .cache_capacity(NonZeroUsize::new(4).expect("nonzero"))
            .build();

        let key = ResourceKey::new("segment-0.m4s");
        let ctx = DrmCtx { xor_key: 0x42 };
        let plaintext = b"hello drm world";
        let ciphertext: Vec<u8> = plaintext.iter().map(|b| b ^ 0x42).collect();

        // Writer A: acquire + write ciphertext + commit.
        {
            let a = store
                .acquire_resource_with_ctx(&key, Some(ctx.clone()))
                .expect("writer A acquire");
            a.write_at(0, &ciphertext).expect("writer A write");
            a.commit(Some(ciphertext.len() as u64))
                .expect("writer A commit");
        }

        // Sanity: availability + a plain open_resource see the
        // committed plaintext at this point.
        assert!(
            store.contains_range(&key, 0..ciphertext.len() as u64),
            "availability must advertise the committed range before \
             the writer B reactivate"
        );

        // Reader holds a clone over the committed DRM entry.
        let reader = store
            .open_resource(&key)
            .expect("reader open_resource after commit");

        let mut probe = vec![0u8; ciphertext.len()];
        let n = reader
            .read_at(0, &mut probe)
            .expect("reader sanity read before writer B");
        assert_eq!(n, ciphertext.len());
        assert_eq!(
            &probe[..],
            plaintext,
            "reader must see decrypted plaintext before any \
             reactivate race"
        );

        // Writer B: `acquire_resource_with_ctx(key, Some(ctx))` with
        // the same ctx. The `CachingAssets` cache is still holding
        // the committed entry from writer A, so this hits the
        // cache-hit branch and calls `res.reactivate()` on the shared
        // `ProcessedResource`. That flips `processed` to `false`.
        let _writer_b = store
            .acquire_resource_with_ctx(&key, Some(ctx.clone()))
            .expect("writer B reacquire");

        // Availability still advertises the committed range — the
        // LRU displace did not fire, so `on_invalidated` did not run
        // and `availability.remove(key)` was never called.
        assert!(
            store.contains_range(&key, 0..ciphertext.len() as u64),
            "availability must still advertise the range after a \
             cache-hit reactivate (no LRU displace occurred)"
        );

        // Reader proceeds with its read_at. Under the current
        // behaviour this triggers the pre-commit guard and returns
        // `StorageError::Failed("processed resource is not readable
        // before commit")`. That hard error is what the HLS source
        // path converts into a `StreamError::Source(..)`, poisoning
        // the decoder FSM and producing the DRM small-cache flake.
        //
        // Acceptable correct behaviours:
        //   (a) read_at succeeds with the already-committed plaintext
        //       bytes (preferred — the reader has a snapshot Arc and
        //       writer B has not yet overwritten), OR
        //   (b) read_at returns a NotFound-style error that the HLS
        //       reader classifies as Retry.
        //
        // Unacceptable: read_at returns `StorageError::Failed` whose
        // message contains "not readable before commit".
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
    #[kithara::test(timeout(kithara_platform::time::Duration::from_secs(5)))]
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

        // Give the reader long enough to enter the gate's wait loop.
        std::thread::sleep(std::time::Duration::from_millis(50));
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
    #[kithara::test(timeout(kithara_platform::time::Duration::from_secs(5)))]
    fn wait_range_aborts_on_cancellation() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x00, Arc::clone(&call_count));

        // Build a resource with an explicit cancel token we can fire
        // from the test thread; the standard `mock_resource` helper
        // immediately drops its token, which would race the assert.
        let dir = tempdir().unwrap();
        let path = dir.path().join("cancel.bin");
        let cancel = CancellationToken::new();
        let resource: MmapResource =
            Resource::open(cancel.clone(), MmapOptions::new(path)).unwrap();
        resource.write_at(0, &[1u8; 16]).unwrap();

        let processed = ProcessedResource::new(resource, Some(()), process_fn, test_pool());
        let processed_for_reader = processed.clone();

        let reader = std::thread::spawn(move || processed_for_reader.wait_range(0..16));

        std::thread::sleep(std::time::Duration::from_millis(50));
        cancel.cancel();

        let outcome = reader
            .join()
            .expect("reader thread panicked")
            .expect("wait_range must not surface a hard error on cancel");
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
}
