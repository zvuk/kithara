#![forbid(unsafe_code)]

use std::{ops::Range, path::Path};

use async_trait::async_trait;
use bytes::Bytes;

use crate::StorageResult;

/// Result of waiting for a byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The requested range is available for reading.
    Ready,
    /// The resource has been committed and the requested range starts at/after EOF.
    Eof,
}

/// Base contract for logical resources in Kithara.
///
/// This trait is intentionally small:
/// - `read`/`write` cover the small-object path (index, playlists, keys).
/// - Lifecycle is explicit (`commit`/`fail`) to unblock waiters deterministically.
///
/// Random-access + waitable-range semantics live in [`StreamingResourceExt`].
#[async_trait]
pub trait Resource: Send + Sync + 'static {
    /// Atomically replace the entire content with `data`.
    async fn write(&self, data: &[u8]) -> StorageResult<()>;

    /// Read the entire content.
    async fn read(&self) -> StorageResult<Bytes>;

    /// Mark the resource as successfully completed.
    ///
    /// - For streaming resources, this seals the resource and defines EOF when `final_len` is known.
    /// - For atomic resources, this may be a no-op (writes are already whole-object).
    async fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;

    /// Mark the resource as failed, waking all waiters.
    ///
    /// Implementations should store the error message so subsequent operations fail consistently.
    async fn fail(&self, error: impl Into<String> + Send) -> StorageResult<()>;

    /// Return the path to the backing file.
    ///
    /// For disk-backed resources, this returns the filesystem path.
    /// For in-memory or network-backed resources, this may return a placeholder path.
    fn path(&self) -> &Path;
}

/// Extension trait for streaming-like resources: random-access reads/writes + waitable ranges.
///
/// This is the contract used for large resources that are filled in pieces (HTTP Range) while
/// being read by a player that may seek at any time.
#[async_trait]
pub trait StreamingResourceExt: Resource {
    /// Wait until the requested `range` becomes readable, or EOF/failure/cancellation occurs.
    ///
    /// Normative semantics:
    /// - If the full range is already available: returns `Ok(WaitOutcome::Ready)` immediately.
    /// - If the resource is committed with a known EOF and `range.start >= eof`: returns `Eof`.
    /// - If the resource fails: returns `StorageError::Failed`.
    /// - If the resource is cancelled: returns `StorageError::Cancelled`.
    async fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;

    /// Read up to `len` bytes at `offset` **without** implicitly waiting.
    ///
    /// Callers that need blocking semantics must call `wait_range` first.
    ///
    /// Implementations should return:
    /// - `Ok(Bytes::new())` for EOF when committed and `offset >= final_len` (when known).
    async fn read_at(&self, offset: u64, len: usize) -> StorageResult<Bytes>;

    /// Write bytes at the given offset (HTTP Range response writes).
    async fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
}

/// Marker extension trait for atomic small-object resources.
///
/// We keep this as a separate trait to make it explicit at the type level when a resource is
/// intended to be used only via whole-object `read`/`write`.
pub trait AtomicResourceExt: Resource {}
