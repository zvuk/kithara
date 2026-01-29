#![forbid(unsafe_code)]

//! Unified resource trait for storage.
//!
//! `ResourceExt` is a sync trait covering both streaming (incremental write) and
//! atomic (whole-file) use-cases. Convenience methods `read_all` and `write_all`
//! build on top of `read_at` / `write_at` + `commit`.

use std::{ops::Range, path::Path};

use bytes::Bytes;

use crate::StorageResult;

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
/// For atomic: use `write_all` / `read_all` convenience methods.
pub trait ResourceExt: Send + Sync + Clone + 'static {
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
    /// If `final_len` is provided, the backing file may be truncated to that size.
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;

    /// Mark the resource as failed.
    fn fail(&self, reason: String);

    /// Get the file path.
    fn path(&self) -> &Path;

    /// Get the committed length, if known.
    fn len(&self) -> Option<u64>;

    /// Returns `true` if the resource has been committed with zero length.
    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Get resource status.
    fn status(&self) -> ResourceStatus;

    // ---- Convenience methods (atomic-style) ----

    /// Read the entire resource contents.
    ///
    /// Returns empty `Bytes` if resource has no data.
    fn read_all(&self) -> StorageResult<Bytes> {
        let len = match self.len() {
            Some(l) => l,
            None => return Ok(Bytes::new()),
        };
        if len == 0 {
            return Ok(Bytes::new());
        }
        let mut buf = vec![0u8; len as usize];
        let n = self.read_at(0, &mut buf)?;
        buf.truncate(n);
        Ok(Bytes::from(buf))
    }

    /// Write entire contents and commit atomically.
    fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        self.write_at(0, data)?;
        self.commit(Some(data.len() as u64))
    }
}
