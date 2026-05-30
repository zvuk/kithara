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
    acquisition::{AcquisitionResult, RawWriteHandle, ReadSide, WriteSide},
    base::Assets,
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

/// Pairs a `processed` flag with a [`Condvar`] so readers can block
/// until [`ProcessedWriter::commit`] drains the processor.
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

/// RAII guard that fails the readiness gate if a writer is dropped without
/// `commit`/`fail`. Lives as a field so [`ProcessedWriter`] itself has no
/// `Drop` impl — `commit`/`fail` partially move `inner` out of the writer,
/// which a `Drop` type forbids (mirrors G-1's payload-Drop split).
struct GateGuard {
    readiness: Arc<ReadinessGate>,
    armed: bool,
}

impl GateGuard {
    fn new(readiness: Arc<ReadinessGate>) -> Self {
        Self {
            readiness,
            armed: true,
        }
    }

    /// Disarm: a clean `commit`/`fail` already settled the gate.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        if self.armed {
            self.readiness.fail();
        }
    }
}

/// Stream `inner`'s raw bytes through `process` chunk-by-chunk and write the
/// transformed bytes back in place. Returns the processed byte count, which may
/// shrink below `final_len` (e.g. PKCS7 unpadding). Reads through a transient
/// reader view of the writer's own generation (raw storage bytes, no gate).
fn run_process<W, Ctx>(
    inner: &W,
    ctx: &Ctx,
    process: &ProcessChunkFn<Ctx>,
    pool: &BytePool,
    final_len: u64,
) -> StorageResult<u64>
where
    W: WriteSide,
    Ctx: Clone,
{
    let mut ctx = ctx.clone();
    let raw = inner.reader();

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

        let n = raw.read_at(read_offset, &mut input_buf[..to_read])?;
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

/// Pending (writer) phase of a processed resource — the sole producer handle.
///
/// Owns the write+decrypt capability and a fresh per-generation
/// [`ReadinessGate`]. Has **no read methods**, so reading a not-yet-committed
/// handle is a compile error. [`commit`](WriteSide::commit) consumes the writer
/// into a [`ProcessedReader`]; dropping without `commit`/`fail` fails the gate
/// so a waiting reader cannot deadlock. See the crate `README.md`.
pub struct ProcessedWriter<W, Ctx> {
    guard: GateGuard,
    pool: BytePool,
    ctx: Option<Ctx>,
    process: ProcessChunkFn<Ctx>,
    inner: W,
}

/// Ready (reader) phase of a processed resource — a cheap-to-clone read view.
///
/// Reads return already-processed bytes. It carries its generation's
/// [`ReadinessGate`]: a view shared with an in-flight writer (via
/// [`WriteSide::reader`]) still blocks in [`wait_range`](ReadSide::wait_range)
/// until that writer commits. [`reactivate`](ReadSide::reactivate) consumes the
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

impl<W: Debug, Ctx: Debug> Debug for ProcessedWriter<W, Ctx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedWriter")
            .field("inner", &self.inner)
            .field("ctx", &self.ctx)
            .field("ready", &self.guard.readiness.is_ready())
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

impl<W, Ctx> ProcessedWriter<W, Ctx>
where
    W: WriteSide,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    /// Create a fresh pending writer. The gate starts ready only when no
    /// processing is required (`ctx` is `None`); an encrypted writer is pending
    /// until [`commit`](WriteSide::commit).
    pub fn new(inner: W, ctx: Option<Ctx>, process: ProcessChunkFn<Ctx>, pool: BytePool) -> Self {
        let ready = ctx.is_none();
        Self {
            inner,
            ctx,
            process,
            pool,
            guard: GateGuard::new(Arc::new(ReadinessGate::new(ready))),
        }
    }

    fn build_reader(&self, inner: W::Reader) -> ProcessedReader<W::Reader, Ctx> {
        ProcessedReader {
            readiness: Arc::clone(&self.guard.readiness),
            pool: self.pool.clone(),
            ctx: self.ctx.clone(),
            process: Arc::clone(&self.process),
            inner,
        }
    }
}

impl<W, Ctx> WriteSide for ProcessedWriter<W, Ctx>
where
    W: WriteSide,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    type Reader = ProcessedReader<W::Reader, Ctx>;

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn reader(&self) -> ProcessedReader<W::Reader, Ctx> {
        self.build_reader(self.inner.reader())
    }

    fn raw_write_handle(&self) -> RawWriteHandle {
        self.inner.raw_write_handle()
    }

    fn commit(mut self, final_len: Option<u64>) -> StorageResult<ProcessedReader<W::Reader, Ctx>> {
        let needs_processing = self.ctx.is_some() && !self.guard.readiness.is_ready();
        let actual_len = match (needs_processing, final_len, self.ctx.as_ref()) {
            (true, Some(len), Some(ctx)) if len > 0 => Some(run_process(
                &self.inner,
                ctx,
                &self.process,
                &self.pool,
                len,
            )?),
            _ => final_len,
        };

        let reader_inner = self.inner.commit(actual_len)?;
        if needs_processing {
            self.guard.readiness.mark_ready();
        }
        self.guard.disarm();
        Ok(ProcessedReader {
            readiness: Arc::clone(&self.guard.readiness),
            pool: self.pool.clone(),
            ctx: self.ctx.clone(),
            process: Arc::clone(&self.process),
            inner: reader_inner,
        })
    }

    fn fail(mut self, reason: String) {
        self.inner.fail(reason);
        self.guard.readiness.fail();
        self.guard.disarm();
    }
}

impl<R, Ctx> ProcessedReader<R, Ctx>
where
    R: ReadSide,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    /// Wrap a Ready (committed or in-flight shared) inner reader. The gate is
    /// open when no processing is required (`ctx` is `None`) or the inner
    /// resource is already committed (its on-disk bytes are already processed).
    fn wrap_ready(
        inner: R,
        ctx: Option<Ctx>,
        process: ProcessChunkFn<Ctx>,
        pool: BytePool,
    ) -> Self {
        let ready = ctx.is_none() || matches!(inner.status(), ResourceStatus::Committed { .. });
        Self {
            readiness: Arc::new(ReadinessGate::new(ready)),
            pool,
            ctx,
            process,
            inner,
        }
    }

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
}

impl<R, Ctx> ReadSide for ProcessedReader<R, Ctx>
where
    R: ReadSide,
    Ctx: Clone + Send + Sync + Debug + 'static,
{
    type Writer = ProcessedWriter<R::Writer, Ctx>;

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

    fn reactivate(self) -> StorageResult<ProcessedWriter<R::Writer, Ctx>> {
        let inner = self.inner.reactivate()?;
        let ready = self.ctx.is_none();
        Ok(ProcessedWriter {
            guard: GateGuard::new(Arc::new(ReadinessGate::new(ready))),
            pool: self.pool,
            ctx: self.ctx,
            process: self.process,
            inner,
        })
    }
}

/// Decorator that applies processing to resources based on context.
///
/// When acquiring/opening a resource with context (Some), wraps the inner
/// handle in a processed writer/reader that decrypts on commit. Without context
/// (None) the resource passes through unprocessed.
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

    fn wrap_ready(
        &self,
        inner: A::ReadyRes,
        ctx: Option<Ctx>,
    ) -> ProcessedReader<A::ReadyRes, Ctx> {
        ProcessedReader::wrap_ready(inner, ctx, Arc::clone(&self.process), self.pool.clone())
    }
}

impl<A, Ctx> Assets for ProcessingAssets<A, Ctx>
where
    A: Assets,
    A::Context: Default,
    Ctx: Clone + Hash + Eq + Send + Sync + Default + Debug + 'static,
{
    type ActiveRes = ProcessedWriter<A::ActiveRes, Ctx>;
    type Context = Ctx;
    type IndexRes = A::IndexRes;
    type ReadyRes = ProcessedReader<A::ReadyRes, Ctx>;

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
        match self.inner.acquire_resource(key, identity)? {
            AcquisitionResult::Pending(w) => Ok(AcquisitionResult::Pending(ProcessedWriter::new(
                w,
                ctx,
                Arc::clone(&self.process),
                self.pool.clone(),
            ))),
            AcquisitionResult::Ready(r) => Ok(AcquisitionResult::Ready(self.wrap_ready(r, ctx))),
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::ReadyRes> {
        Ok(self.wrap_ready(self.inner.open_resource(key, identity)?, ctx))
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

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_storage::{MmapOptions, MmapResource, Resource, StorageResource};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        AssetStoreBuilder,
        base::{BaseReader, BaseWriter},
    };

    fn test_pool() -> BytePool {
        BytePool::new(4, Consts::CHUNK_SIZE)
    }

    /// Simple mock writer for testing, backed by a fresh mmap resource.
    /// Returns both the writer and the `TempDir` to keep the directory alive.
    fn mock_writer(content: &[u8]) -> (BaseWriter, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let cancel = CancellationToken::new();

        let res: MmapResource = Resource::open(cancel, MmapOptions::new(path)).unwrap();
        res.write_at(0, content).unwrap();
        (BaseWriter::new(StorageResource::from(res)), dir)
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
    fn writer_commit_returns_readable_reader() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let (writer, _dir) = mock_writer(b"test content");

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
        let reader = writer.commit(Some(b"test content".len() as u64)).unwrap();
        assert!(call_count.load(Ordering::SeqCst) > 0);

        let mut buf = vec![0u8; 12];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 12);
        let expected: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(buf, expected);
    }

    #[kithara::test]
    fn read_at_after_processing_honours_offset() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0xFF, Arc::clone(&call_count));
        let content: Vec<u8> = (0..100).collect();
        let (writer, _dir) = mock_writer(&content);

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
        let reader = writer.commit(Some(100)).unwrap();

        let mut buf = vec![0u8; 20];
        let n = reader.read_at(40, &mut buf).unwrap();
        assert_eq!(n, 20);
        let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
        assert_eq!(buf, expected);
    }

    #[kithara::test]
    fn ctx_none_writer_commits_straight_through_to_readable() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let (writer, _dir) = mock_writer(b"plain bytes!");

        let writer: ProcessedWriter<BaseWriter, ()> =
            ProcessedWriter::new(writer, None, process_fn, test_pool());
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

    #[kithara::test]
    fn encrypted_writer_reader_is_not_readable_before_commit() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let (writer, _dir) = mock_writer(b"test content");

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
        let reader = writer.reader();

        assert!(
            !reader.contains_range(0..12),
            "encrypted active resource must not advertise readable ranges before processing"
        );
        let mut buf = vec![0u8; 12];
        let err = reader
            .read_at(0, &mut buf)
            .expect_err("encrypted active resource must reject reads before commit");
        assert!(matches!(err, StorageError::NotReadable));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn reader_view_blocks_until_writer_commits() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x55, Arc::clone(&call_count));
        let raw: Vec<u8> = (0..32u8).collect();
        let (writer, _dir) = mock_writer(&raw);

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
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

        let writer = ProcessedWriter::new(
            BaseWriter::new(StorageResource::from(resource)),
            Some(()),
            process_fn,
            test_pool(),
        );
        let reader = writer.reader();

        let handle = std::thread::spawn(move || reader.wait_range(0..16));

        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();

        let outcome = handle
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
        // keep the writer alive until after the assertion so the abort is
        // attributable to cancellation, not a drop-fail.
        drop(writer);
    }

    #[kithara::test]
    fn reactivate_forks_fresh_gate_without_poisoning_reader() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let plaintext = b"hello drm world";
        let ciphertext: Vec<u8> = plaintext.iter().map(|b| b ^ 0x42).collect();
        let (writer, _dir) = mock_writer(&ciphertext);

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
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
        let (writer, _dir) = mock_writer(&first);

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
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
        let (writer, _dir) = mock_writer(&[7u8; 16]);

        let writer = ProcessedWriter::new(writer, Some(()), process_fn, test_pool());
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

    /// Reopening a previously-committed processed resource must read as
    /// plaintext immediately, without re-running the processor — the on-disk
    /// bytes are already decrypted.
    #[kithara::test]
    fn reopened_committed_processed_reader_is_readable_immediately() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let already_processed: Vec<u8> = b"test content".iter().map(|b| b ^ 0x42).collect();

        let dir = tempdir().unwrap();
        let path = dir.path().join("reopen.bin");
        let res: MmapResource =
            Resource::open(CancellationToken::new(), MmapOptions::new(path)).unwrap();
        res.write_at(0, &already_processed).unwrap();
        let storage = StorageResource::from(res);
        storage
            .commit(Some(already_processed.len() as u64))
            .unwrap();

        let reader = ProcessedReader::wrap_ready(
            BaseReader::new(storage),
            Some(()),
            process_fn,
            test_pool(),
        );

        let mut buf = vec![0u8; already_processed.len()];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, already_processed.len());
        assert_eq!(buf, already_processed);
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
    }

    /// Cache fall-through: `open_resource(key)` (ctx=None) must not return
    /// an uncommitted DRM-style entry whose read trips the pre-commit guard.
    #[kithara::test]
    fn open_resource_none_ctx_does_not_leak_precommit_guard() {
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

        let AcquisitionResult::Pending(writer) = store
            .acquire_resource_with_ctx(&key, None, Some(ctx.clone()))
            .expect("BUG: acquire with ctx must succeed")
        else {
            panic!("fresh acquire must be Pending");
        };
        let payload = b"uncommitted encrypted bytes";
        writer
            .write_at(0, payload)
            .expect("BUG: writer must be able to stream bytes before commit");

        if let Ok(reader) = store.open_resource(&key, None) {
            let mut buf = vec![0u8; payload.len()];
            if let Err(err) = reader.read_at(0, &mut buf) {
                assert!(
                    !matches!(err, StorageError::NotReadable),
                    "read_at(ctx=None) leaked the pre-commit guard from a \
                     concurrent ctx=Some writer"
                );
            }
        }
    }
}
