#![forbid(unsafe_code)]

use std::{fmt, fmt::Debug, ops::Range, path::Path, sync::Arc};

use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

/// A clone-able raw-byte write handle, decoupled from the non-`Clone` commit
/// owner. A streaming download closure holds one to write *pre-processing*
/// (e.g. ciphertext) bytes into the backing storage while the [`WriteSide`]
/// writer retains sole ownership of [`commit`](WriteSide::commit). Writes land
/// on the same generation the writer will commit.
#[derive(Clone)]
pub struct RawWriteHandle(Arc<dyn Fn(u64, &[u8]) -> StorageResult<()> + Send + Sync>);

impl RawWriteHandle {
    /// Build a handle from a raw write closure.
    pub fn new(f: impl Fn(u64, &[u8]) -> StorageResult<()> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Write `data` at `offset` to the backing storage.
    ///
    /// # Errors
    /// Propagates the backing write error.
    pub fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        (self.0)(offset, data)
    }
}

impl Debug for RawWriteHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawWriteHandle")
    }
}

/// Outcome of an acquire/open through the [`Assets`](crate::Assets) stack.
///
/// The phase is carried in the type, not a runtime flag: `Pending` hands back a
/// write-only [`WriteSide`] handle that must [`commit`](WriteSide::commit)
/// before any read, `Ready` hands back a read-only [`ReadSide`] handle. Callers
/// pattern-match; there is no runtime `is_readable()` probe.
#[derive(Debug)]
#[non_exhaustive]
pub enum AcquisitionResult<A, R> {
    /// Resource is being produced — the writer must `commit` to make it readable.
    Pending(A),
    /// Resource is readable now.
    Ready(R),
}

impl<A, R> AcquisitionResult<A, R> {
    /// `true` when the resource is already readable.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

/// Read capability of a resource handle — the `Ready` phase.
///
/// Cheap to clone (a shared read view). Decorator wrappers
/// ([`CachedResource`](crate::CachedResource), [`LeaseResource`](crate::LeaseResource))
/// delegate through this trait to stay generic over the inner reader.
pub trait ReadSide: Clone + Send + Sync + Debug + 'static {
    /// The writer phase produced by [`reactivate`](Self::reactivate).
    type Writer: WriteSide<Reader = Self>;

    /// Read already-readable bytes at `offset`.
    ///
    /// # Errors
    /// [`StorageError::NotReadable`](kithara_storage::StorageError::NotReadable)
    /// while a shared in-flight generation has not committed; otherwise the
    /// backing read error.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;

    /// Wait until `range` is available (and, for processed resources, processed).
    ///
    /// # Errors
    /// Propagates the backing wait error.
    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;

    /// Whether `range` is fully readable.
    fn contains_range(&self, range: Range<u64>) -> bool;

    /// Committed length, if known.
    fn len(&self) -> Option<u64>;

    /// Whether the resource committed with zero length.
    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// First gap in available data starting at `from`, up to `limit`.
    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        let Some(len) = self.len() else {
            let mut probe = [0u8; 1];
            let _ = self.read_at(0, &mut probe)?;
            return Ok(0);
        };
        if len == 0 {
            buf.clear();
            return Ok(0);
        }
        let len_usize = usize::try_from(len).unwrap_or(usize::MAX);
        buf.resize(len_usize, 0);
        let n = self.read_at(0, buf)?;
        buf.truncate(n);
        Ok(n)
    }

    /// Backing file path, if any.
    fn path(&self) -> Option<&Path>;

    /// Current runtime lifecycle status of the backing resource.
    fn status(&self) -> ResourceStatus;

    /// Consume this reader and reopen the backing resource for writing, minting
    /// a **fresh** generation (a new readiness gate). Other clones of this
    /// reader keep their own generation and are never poisoned.
    ///
    /// # Errors
    /// Propagates the backing reactivate error.
    fn reactivate(self) -> StorageResult<Self::Writer>;
}

/// Write capability of a resource handle — the `Pending` phase.
///
/// **Not `Clone`**: a single producer owns it and consumes it on
/// [`commit`](Self::commit) / [`fail`](Self::fail). Has no read methods, so
/// reading a not-yet-committed handle is a compile error.
pub trait WriteSide: Send + Sync + Debug + 'static {
    /// The reader phase produced by [`commit`](Self::commit).
    type Reader: ReadSide;

    /// Stream raw (pre-processing) bytes to the backing resource.
    ///
    /// # Errors
    /// Propagates the backing write error.
    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;

    /// A shared read view of this writer's generation. The view is `Clone`;
    /// the writer is not. For a processed (encrypted) writer the view blocks in
    /// [`wait_range`](ReadSide::wait_range) until this writer
    /// [`commit`](Self::commit)s.
    fn reader(&self) -> Self::Reader;

    /// A clone-able raw-write handle for streaming pre-processing bytes into
    /// this writer's generation (see [`RawWriteHandle`]). Lets a `'static`
    /// download closure write while the writer keeps sole ownership of
    /// `commit`.
    fn raw_write_handle(&self) -> RawWriteHandle;

    /// Finalize the resource (running any processing) and consume the writer
    /// into a [`ReadSide`] reader.
    ///
    /// # Errors
    /// Propagates processing or backing-commit errors.
    fn commit(self, final_len: Option<u64>) -> StorageResult<Self::Reader>;

    /// Mark the resource failed, waking any waiting reader.
    fn fail(self, reason: String);
}
