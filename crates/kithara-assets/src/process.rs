#![forbid(unsafe_code)]

use std::{
    fmt,
    fmt::Debug,
    ops::Range,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use kithara_bufpool::BytePool;
use kithara_platform::{
    sync::{Arc, CondvarGate},
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

/// Per-acquire resource processor: an immutable identity plus a factory for
/// fresh mutable chaining state. `identity()` is the exact (non-hashed) cache
/// identity (e.g. AES `key||iv`); `begin()` mints a fresh [`ChunkSink`] per
/// commit so chaining state (e.g. a CBC IV) restarts from the seed.
pub trait ResourceProcessor: Send + Sync + Debug {
    /// Mint a fresh mutable chaining state for one commit.
    fn begin(&self) -> Box<dyn ChunkSink>;

    /// Immutable byte identity of this processor; exact `Eq` for the cache key.
    fn identity(&self) -> &[u8];
}

/// Mutable per-commit chaining state driven chunk-by-chunk on commit.
///
/// Holds whatever evolves between 64KB chunks (e.g. a CBC IV).
pub trait ChunkSink: Send {
    /// Transform `input` into `output` (same length). `is_last` flags the final
    /// chunk so block ciphers can apply padding. Returns the bytes written.
    ///
    /// # Errors
    /// Returns a message string if the transform fails (e.g. misaligned input).
    fn process(&mut self, input: &[u8], output: &mut [u8], is_last: bool) -> Result<usize, String>;
}

/// Per-acquire processing handle: a shared [`ResourceProcessor`] trait object.
/// `None` at an acquire site is identity passthrough (playlists, keys, init).
pub type ProcessCtx = Arc<dyn ResourceProcessor>;

/// Pairs a `processed` flag with the shared [`CondvarGate`] so readers can
/// block until [`ProcessedWriter::commit`] drains the processor.
///
/// The gate guards the `processed` bool directly (single-lock event-driven
/// wait); the guard is only ever held inside [`ReadinessGate::wait_until_ready`]
/// or the brief flip in [`ReadinessGate::mark_ready`].
struct ReadinessGate {
    failed: AtomicBool,
    gate: CondvarGate<bool>,
}

impl ReadinessGate {
    fn new(initial: bool) -> Self {
        Self {
            gate: CondvarGate::new(initial),
            failed: AtomicBool::new(false),
        }
    }

    /// Fail the gate: wake every waiter so a writer dropped without
    /// `commit` cannot deadlock a reader parked in [`Self::wait_until_ready`].
    /// Waiters observe [`Self::is_failed`] and abort. `processed` is left
    /// untouched so [`Self::is_ready`] stays `false` — a failed (never
    /// committed) resource must not read as valid via `read_at`.
    fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.gate.notify_all();
    }

    fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    fn is_ready(&self) -> bool {
        *self.gate.lock()
    }

    /// Mark the gate ready and wake every waiter.
    fn mark_ready(&self) {
        *self.gate.lock() = true;
        self.gate.notify_all();
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
                let guard = self.gate.lock();
                if *guard {
                    return !self.is_failed();
                }
                let deadline = Instant::now() + Duration::from_millis(COND_WAIT_MS);
                let next = self.gate.wait_until(guard, deadline);
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
/// shrink below `final_len` (e.g. PKCS7 unpadding). Reads via
/// [`WriteSide::read_inflight_at`] — the writer's own in-flight working storage,
/// never a committed snapshot kept published for concurrent readers during a
/// re-download (which would feed the processor the prior generation's bytes).
/// Borrowed inputs for [`run_process`]: the `inner` write side, the
/// `processor` to mint a fresh sink from, the byte `pool`, and the
/// `final_len` to process up to.
struct ProcessArgs<'a, W> {
    pool: &'a BytePool,
    processor: &'a ProcessCtx,
    inner: &'a W,
    final_len: u64,
}

fn run_process<W>(args: &ProcessArgs<'_, W>) -> StorageResult<u64>
where
    W: WriteSide,
{
    let &ProcessArgs {
        inner,
        processor,
        pool,
        final_len,
    } = args;
    let mut sink = processor.begin();
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

        let n = raw.read_inflight_at(read_offset, &mut input_buf[..to_read])?;
        if n == 0 {
            break;
        }

        let written = sink
            .process(&input_buf[..n], &mut output_buf[..n], is_last)
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
/// so a waiting reader cannot deadlock. See the crate `CONTEXT.md`.
pub struct ProcessedWriter<W> {
    pool: BytePool,
    guard: GateGuard,
    processor: Option<ProcessCtx>,
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
pub struct ProcessedReader<R> {
    readiness: Arc<ReadinessGate>,
    pool: BytePool,
    processor: Option<ProcessCtx>,
    inner: R,
}

impl<R> Clone for ProcessedReader<R>
where
    R: Clone,
{
    fn clone(&self) -> Self {
        Self {
            readiness: Arc::clone(&self.readiness),
            pool: self.pool.clone(),
            processor: self.processor.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<W: Debug> Debug for ProcessedWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedWriter")
            .field("inner", &self.inner)
            .field("processor", &self.processor)
            .field("ready", &self.guard.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

impl<R: Debug> Debug for ProcessedReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessedReader")
            .field("inner", &self.inner)
            .field("processor", &self.processor)
            .field("ready", &self.readiness.is_ready())
            .finish_non_exhaustive()
    }
}

impl<W> ProcessedWriter<W>
where
    W: WriteSide,
{
    /// Create a fresh pending writer. The gate starts ready only when no
    /// processing is required (`processor` is `None`); an encrypted writer is
    /// pending until [`commit`](WriteSide::commit).
    pub fn new(inner: W, processor: Option<ProcessCtx>, pool: BytePool) -> Self {
        let ready = processor.is_none();
        Self {
            inner,
            processor,
            pool,
            guard: GateGuard::new(Arc::new(ReadinessGate::new(ready))),
        }
    }

    fn build_reader(&self, inner: W::Reader) -> ProcessedReader<W::Reader> {
        ProcessedReader {
            inner,
            readiness: Arc::clone(&self.guard.readiness),
            pool: self.pool.clone(),
            processor: self.processor.clone(),
        }
    }
}

impl<W> WriteSide for ProcessedWriter<W>
where
    W: WriteSide,
{
    type Reader = ProcessedReader<W::Reader>;

    fn commit(mut self, final_len: Option<u64>) -> StorageResult<ProcessedReader<W::Reader>> {
        let needs_processing = self.processor.is_some() && !self.guard.readiness.is_ready();
        let actual_len = match (needs_processing, final_len, self.processor.as_ref()) {
            (true, Some(len), Some(processor)) if len > 0 => Some(run_process(&ProcessArgs {
                processor,
                inner: &self.inner,
                pool: &self.pool,
                final_len: len,
            })?),
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
            processor: self.processor.clone(),
            inner: reader_inner,
        })
    }

    fn fail(mut self, reason: String) {
        self.inner.fail(reason);
        self.guard.readiness.fail();
        self.guard.disarm();
    }

    delegate::delegate! {
        to self.inner {
            fn raw_write_handle(&self) -> RawWriteHandle;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }

    fn reader(&self) -> ProcessedReader<W::Reader> {
        self.build_reader(self.inner.reader())
    }
}

impl<R> ProcessedReader<R>
where
    R: ReadSide,
{
    /// A waiter must abort when the gate failed (writer dropped) or the backing
    /// resource reached a terminal state — neither will ever flip ready.
    fn inner_terminal(&self) -> bool {
        self.readiness.is_failed()
            || matches!(
                self.inner.status(),
                ResourceStatus::Failed(_) | ResourceStatus::Cancelled
            )
    }

    fn is_readable(&self) -> bool {
        self.processor.is_none() || self.readiness.is_ready()
    }

    /// Wrap a Ready (committed or in-flight shared) inner reader. The gate is
    /// open when no processing is required (`processor` is `None`) or the inner
    /// resource is already committed (its on-disk bytes are already processed).
    fn wrap_ready(inner: R, processor: Option<ProcessCtx>, pool: BytePool) -> Self {
        let ready =
            processor.is_none() || matches!(inner.status(), ResourceStatus::Committed { .. });
        Self {
            pool,
            processor,
            inner,
            readiness: Arc::new(ReadinessGate::new(ready)),
        }
    }
}

impl<R> ReadSide for ProcessedReader<R>
where
    R: ReadSide,
{
    type Writer = ProcessedWriter<R::Writer>;

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.is_readable() && self.inner.contains_range(range)
    }

    delegate::delegate! {
        to self.inner {
            fn len(&self) -> Option<u64>;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
            fn path(&self) -> Option<&Path>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn status(&self) -> ResourceStatus;
        }
    }

    fn reactivate(self) -> StorageResult<ProcessedWriter<R::Writer>> {
        let inner = self.inner.reactivate()?;
        let ready = self.processor.is_none();
        Ok(ProcessedWriter {
            inner,
            guard: GateGuard::new(Arc::new(ReadinessGate::new(ready))),
            pool: self.pool,
            processor: self.processor,
        })
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if !self.is_readable() {
            return Err(StorageError::NotReadable);
        }
        self.inner.read_at(offset, buf)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        let outcome = self.inner.wait_range(range)?;
        if self.processor.is_none() || outcome != WaitOutcome::Ready {
            return Ok(outcome);
        }
        if self.readiness.wait_until_ready(&|| self.inner_terminal()) {
            Ok(WaitOutcome::Ready)
        } else {
            Ok(WaitOutcome::Interrupted)
        }
    }
}

/// Decorator that applies processing to resources based on context.
///
/// When acquiring/opening a resource with context (Some), wraps the inner
/// handle in a processed writer/reader that decrypts on commit. Without context
/// (None) the resource passes through unprocessed.
#[derive(Clone)]
pub struct ProcessingAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    pool: BytePool,
}

impl<A> ProcessingAssets<A>
where
    A: Assets,
{
    pub fn new(inner: Arc<A>, pool: BytePool) -> Self {
        Self { inner, pool }
    }

    fn wrap_ready(
        &self,
        inner: A::ReadyRes,
        processor: Option<ProcessCtx>,
    ) -> ProcessedReader<A::ReadyRes> {
        ProcessedReader::wrap_ready(inner, processor, self.pool.clone())
    }
}

impl<A> Assets for ProcessingAssets<A>
where
    A: Assets,
{
    type ActiveRes = ProcessedWriter<A::ActiveRes>;
    type Context = ProcessCtx;
    type IndexRes = A::IndexRes;
    type ReadyRes = ProcessedReader<A::ReadyRes>;

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

    use kithara_platform::{CancelToken, thread};
    use kithara_storage::{
        MemOptions, MemResource, MmapOptions, MmapResource, Resource, StorageResource,
    };
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        AssetStoreBuilder, StorageBackend,
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
        let cancel = CancelToken::never();

        let res: MmapResource = Resource::open(cancel, MmapOptions::new(path)).unwrap();
        res.write_at(0, content).unwrap();
        (BaseWriter::new(StorageResource::from(res)), dir)
    }

    /// Mem-backed mock writer, mirroring [`mock_writer`] for the ephemeral path.
    fn mock_writer_mem(content: &[u8]) -> BaseWriter {
        let cancel = CancelToken::never();
        let res: MemResource = Resource::open(
            cancel,
            MemOptions {
                initial_data: None,
                capacity: 0,
            },
        )
        .unwrap();
        res.write_at(0, content).unwrap();
        BaseWriter::new(StorageResource::from(res))
    }

    /// XOR processor whose `identity()` is the single key byte; each commit
    /// mints a fresh sink that counts its `process` calls.
    #[derive(Debug)]
    struct XorProcessor {
        call_count: Arc<AtomicUsize>,
        identity: Box<[u8]>,
        xor_key: u8,
    }

    struct XorSink {
        call_count: Arc<AtomicUsize>,
        xor_key: u8,
    }

    impl ChunkSink for XorSink {
        fn process(
            &mut self,
            input: &[u8],
            output: &mut [u8],
            _is_last: bool,
        ) -> Result<usize, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            for (i, &b) in input.iter().enumerate() {
                output[i] = b ^ self.xor_key;
            }
            Ok(input.len())
        }
    }

    impl ResourceProcessor for XorProcessor {
        fn begin(&self) -> Box<dyn ChunkSink> {
            Box::new(XorSink {
                xor_key: self.xor_key,
                call_count: Arc::clone(&self.call_count),
            })
        }

        fn identity(&self) -> &[u8] {
            &self.identity
        }
    }

    /// Build a `ProcessCtx` XOR processor (no allocation in the sink).
    fn xor_chunk_processor(xor_key: u8, call_count: Arc<AtomicUsize>) -> ProcessCtx {
        Arc::new(XorProcessor {
            xor_key,
            identity: Box::new([xor_key]),
            call_count,
        })
    }

    #[kithara::test]
    fn writer_commit_returns_readable_reader() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let (writer, _dir) = mock_writer(b"test content");

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
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

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
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

        let _ = process_fn;
        let writer: ProcessedWriter<BaseWriter> = ProcessedWriter::new(writer, None, test_pool());
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

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
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

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
        let reader = writer.reader();
        let raw_len = raw.len() as u64;

        let handle = std::thread::spawn(move || {
            let outcome = reader.wait_range(0..raw_len).unwrap();
            assert_eq!(outcome, WaitOutcome::Ready);
            let mut buf = vec![0u8; 32];
            reader.read_at(0, &mut buf).unwrap();
            buf
        });

        thread::sleep(Duration::from_millis(50)); // M5: real pacing, replace with teardown signal
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
        let cancel = CancelToken::never();
        let resource: MmapResource =
            Resource::open(cancel.clone(), MmapOptions::new(path)).unwrap();
        resource.write_at(0, &[1u8; 16]).unwrap();

        let writer = ProcessedWriter::new(
            BaseWriter::new(StorageResource::from(resource)),
            Some(process_fn),
            test_pool(),
        );
        let reader = writer.reader();

        let handle = std::thread::spawn(move || reader.wait_range(0..16));

        thread::sleep(Duration::from_millis(50)); // M5: real pacing, replace with teardown signal
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

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
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

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
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

    #[kithara::test]
    fn reactivate_then_commit_reruns_processor_mem() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let process_fn = xor_chunk_processor(0x42, Arc::clone(&call_count));
        let first: Vec<u8> = b"first  payload".iter().map(|b| b ^ 0x42).collect();
        let second: Vec<u8> = b"second payload".iter().map(|b| b ^ 0x42).collect();
        assert_eq!(first.len(), second.len());
        let writer = mock_writer_mem(&first);

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
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

        let writer = ProcessedWriter::new(writer, Some(process_fn), test_pool());
        writer.write_at(0, &[7u8; 16]).unwrap();
        let reader = writer.reader();
        let reader_probe = reader.clone();

        let handle = std::thread::spawn(move || reader.wait_range(0..16));
        thread::sleep(Duration::from_millis(50)); // M5: real pacing, replace with teardown signal
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
            Resource::open(CancelToken::never(), MmapOptions::new(path)).unwrap();
        res.write_at(0, &already_processed).unwrap();
        let storage = StorageResource::from(res);
        storage
            .commit(Some(already_processed.len() as u64))
            .unwrap();

        let reader =
            ProcessedReader::wrap_ready(BaseReader::new(storage), Some(process_fn), test_pool());

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
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .build();
        let key = ResourceKey::relative("drm-fallthrough", "segment.m4s");
        let proc: ProcessCtx = xor_chunk_processor(0x42, Arc::new(AtomicUsize::new(0)));

        let AcquisitionResult::Pending(writer) = store
            .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&proc)))
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
