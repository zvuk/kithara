#![forbid(unsafe_code)]

//! Unified storage resource trait and shared types.
//!
//! `ResourceExt` is a sync trait covering both streaming (incremental write) and
//! atomic (whole-file) use-cases.
//!
//! Concrete implementations via [`Resource<D>`](crate::Resource):
//! - [`MmapResource`](crate::MmapResource) — mmap-backed (filesystem).
//! - [`MemResource`](crate::MemResource) — in-memory `Vec<u8>` (WASM).

use std::{ops::Range, path::Path};

use rangemap::RangeSet;

use crate::StorageResult;

/// Controls how [`MmapDriver`](crate::MmapDriver) opens the backing file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenMode {
    /// Auto-detect: existing files open as committed (read-only mmap),
    /// new files open as active (read-write mmap). Default behavior.
    #[default]
    Auto,
    /// Always read-write. Existing files are opened without truncation.
    /// Writes after commit transparently reopen the file as read-write.
    /// Use for files that are rewritten in place (e.g. index files).
    ReadWrite,
    /// Always read-only. Existing files are committed; missing files
    /// are treated as empty committed resources. Writes are rejected.
    ReadOnly,
}

/// Outcome of waiting for a byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The requested range is available for reading.
    Ready,
    /// The resource has been committed and the requested range starts at/after EOF.
    Eof,
}

/// Status of a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceStatus {
    /// Resource is open for writing (streaming in progress).
    Active,
    /// Resource has been committed (all data written).
    Committed { final_len: Option<u64> },
    /// Resource encountered an error.
    Failed(String),
}

/// Unified sync resource trait.
///
/// Covers both incremental streaming (segments, progressive downloads)
/// and atomic whole-file (playlists, keys, indexes) use-cases.
///
/// For streaming: use `write_at` + `commit`.
/// For atomic: use `write_all` / `read_into` convenience methods.
#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = ResourceMock)
)]
pub trait ResourceExt: Send + Sync + 'static {
    /// Read data at the given offset into `buf`.
    ///
    /// Returns the number of bytes read.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;

    /// Write data at the given offset.
    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;

    /// Wait until the given byte range is available.
    ///
    /// Blocks the calling thread using `Condvar` until data is written
    /// or the resource reaches EOF / error / cancellation.
    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;

    /// Mark the resource as fully written.
    ///
    /// If `final_len` is provided, the backing storage may be truncated to that size.
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;

    /// Mark the resource as failed.
    fn fail(&self, reason: String);

    /// Get the file path, if backed by a file.
    ///
    /// Returns `None` for in-memory resources that have no filesystem path.
    fn path(&self) -> Option<&Path>;

    /// Get the committed length, if known.
    fn len(&self) -> Option<u64>;

    /// Returns `true` if the resource has been committed with zero length.
    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Get resource status.
    fn status(&self) -> ResourceStatus;

    /// Reactivate a committed resource for continued writing.
    ///
    /// Transitions Committed → Active: reopens the backing store for writing,
    /// resets committed flag, and clears `final_len`. Existing data remains
    /// available for reading. New data can be written at any offset.
    ///
    /// Use for resuming partial downloads where the resource is smaller
    /// than the expected total.
    ///
    /// No-op if the resource is already Active.
    fn reactivate(&self) -> StorageResult<()>;

    /// Read the entire resource contents into a caller-provided buffer.
    ///
    /// The buffer is resized to fit the data. Returns the number of bytes read.
    /// Returns `0` if resource has no data.
    fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        let Some(len) = self.len() else {
            // Probe via read_at to detect error state (cancelled/failed).
            let mut probe = [0u8; 1];
            let _ = self.read_at(0, &mut probe)?;
            return Ok(0);
        };
        if len == 0 {
            buf.clear();
            return Ok(0);
        }
        buf.resize(len as usize, 0);
        let n = self.read_at(0, buf)?;
        buf.truncate(n);
        Ok(n)
    }

    /// Write entire contents and commit atomically.
    fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        self.write_at(0, data)?;
        self.commit(Some(data.len() as u64))
    }
}

/// Check if the `available` range set fully covers `range`.
pub(crate) fn range_covered_by(available: &RangeSet<u64>, range: &Range<u64>) -> bool {
    if range.is_empty() {
        return true;
    }
    !available.gaps(range).any(|_| true)
}
