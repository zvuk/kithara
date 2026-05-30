#![forbid(unsafe_code)]

use std::{fmt::Debug, ops::Range, path::Path};

use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

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

    /// Backing file path, if any.
    fn path(&self) -> Option<&Path>;

    /// Current runtime lifecycle status of the backing resource.
    fn status(&self) -> ResourceStatus;
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

    /// Finalize the resource (running any processing) and consume the writer
    /// into a [`ReadSide`] reader.
    ///
    /// # Errors
    /// Propagates processing or backing-commit errors.
    fn commit(self, final_len: Option<u64>) -> StorageResult<Self::Reader>;

    /// Mark the resource failed, waking any waiting reader.
    fn fail(self, reason: String);
}
