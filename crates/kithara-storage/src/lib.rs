#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Async, waitable, random-access storage for a *single* logical resource.
//!
//! ## Design goals
//! - Random-access `read_at` / `write_at` (for HTTP Range responses).
//! - Explicit `wait_range` to await availability (no polling / no "false EOF").
//! - Cancellation via `tokio_util::sync::CancellationToken`.
//! - Separate "in progress" vs "committed" lifecycle with `commit(final_len)`.
//!
//! This crate is intentionally scoped to *one* resource. Managing many resources, eviction,
//! leases, and on-disk indexing belongs in a higher-level crate (e.g. `kithara-assets`).

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use rangemap::RangeSet;
use thiserror::Error;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use random_access_disk::RandomAccessDisk;
use random_access_storage::{RandomAccess, RandomAccessError};

pub type StorageResult<T> = Result<T, StorageError>;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("random access error: {0}")]
    RandomAccess(#[from] RandomAccessError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid range: start {start} >= end {end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("resource is sealed (no longer writable)")]
    Sealed,

    #[error("resource failed: {0}")]
    Failed(String),

    #[error("operation cancelled")]
    Cancelled,
}

/// Result of [`Resource::wait_range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The requested range is fully available for reading.
    Ready,
    /// The resource has been committed and the requested range is at/after EOF.
    ///
    /// Readers may treat this as immediate EOF (`Read::read` should return `Ok(0)`).
    Eof,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceStatus {
    InProgress,
    Committed { final_len: Option<u64> },
    Failed,
}

/// Options for opening a disk-backed resource.
#[derive(Clone, Debug)]
pub struct DiskOptions {
    /// Path to the backing file.
    pub path: PathBuf,

    /// Mandatory cancellation token for this resource lifecycle.
    ///
    /// Used by `wait_range` (and write paths) to ensure nothing can hang indefinitely if the
    /// owning session is cancelled/dropped.
    pub cancel: CancellationToken,

    /// Optional initial length hint. If set, the file may be truncated/extended to this size.
    /// This is a *hint*, not a hard contract; callers may later `commit(Some(final_len))`.
    pub initial_len: Option<u64>,
}

impl DiskOptions {
    pub fn new(path: impl Into<PathBuf>, cancel: CancellationToken) -> Self {
        Self {
            path: path.into(),
            cancel,
            initial_len: None,
        }
    }
}

/// A single logical resource that can be written in ranges and read with async waiting.
///
/// Clone is cheap; all clones refer to the same underlying resource.
#[derive(Clone)]
pub struct Resource {
    inner: Arc<Inner>,
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resource")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl Resource {
    /// Open or create a disk-backed resource.
    ///
    /// This does not imply the resource is ready. Callers should write ranges and then `commit`.
    pub async fn open_disk(opts: DiskOptions) -> StorageResult<Self> {
        let mut disk = RandomAccessDisk::open(opts.path.clone()).await?;
        if let Some(len) = opts.initial_len {
            // Best-effort sizing. Some backends may create sparse files; that's fine.
            disk.truncate(len).await?;
        }

        Ok(Self {
            inner: Arc::new(Inner {
                path: opts.path,
                cancel: opts.cancel,
                disk: Mutex::new(disk),
                state: Mutex::new(State::new()),
                notify: Notify::new(),
            }),
        })
    }

    /// Return the backing path (disk backend).
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Return current status (best-effort snapshot).
    pub async fn status(&self) -> ResourceStatus {
        let state = self.inner.state.lock().await;
        if let Some(err) = &state.failed {
            let _ = err;
            return ResourceStatus::Failed;
        }
        if state.sealed {
            return ResourceStatus::Committed {
                final_len: state.final_len,
            };
        }
        ResourceStatus::InProgress
    }

    /// Write bytes at the given offset.
    ///
    /// This is intended for HTTP Range responses. After write succeeds, the written range becomes
    /// available to readers and will wake `wait_range` waiters.
    pub async fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }
        if data.is_empty() {
            return Ok(());
        }

        let end =
            offset
                .checked_add(data.len() as u64)
                .ok_or_else(|| StorageError::InvalidRange {
                    start: offset,
                    end: offset,
                })?;

        {
            let mut state = self.inner.state.lock().await;
            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }
            if state.sealed {
                return Err(StorageError::Sealed);
            }
            // Mark as available after successful write (below).
            state.pending_add = Some(offset..end);
        }

        {
            let mut disk = self.inner.disk.lock().await;
            disk.write(offset, data).await?;
        }

        {
            let mut state = self.inner.state.lock().await;
            if let Some(r) = state.pending_add.take() {
                state.available.insert(r);
            }
        }

        self.inner.notify.notify_waiters();
        Ok(())
    }

    /// Read up to `len` bytes at `offset` without waiting for availability.
    ///
    /// Callers typically should `wait_range` first to ensure data is present.
    pub async fn read_at(&self, offset: u64, len: usize) -> StorageResult<Bytes> {
        if len == 0 {
            return Ok(Bytes::new());
        }

        // If committed with known final length, clamp reads to EOF.
        if let Some(clamped) = self.clamp_len_to_eof(offset, len).await? {
            if clamped == 0 {
                return Ok(Bytes::new());
            }
            return self.read_exact_len(offset, clamped).await;
        }

        self.read_exact_len(offset, len).await
    }

    /// Await until the full `range` is available for reading, or the resource reaches EOF (commit),
    /// or fails, or `token` is cancelled.
    ///
    /// Semantics (normative):
    /// - If the range is already available, returns `Ready` immediately.
    /// - If the resource is committed with a known `final_len` and `range.start >= final_len`,
    ///   returns `Eof` immediately.
    /// - If committed and the range partially overlaps EOF, this waits until the portion before EOF
    ///   is available, then returns `Ready` (reader should clamp read length).
    pub async fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        validate_range(&range)?;

        loop {
            // Fast path: check state under lock.
            {
                let state = self.inner.state.lock().await;

                if let Some(err) = &state.failed {
                    return Err(StorageError::Failed(err.clone()));
                }

                if state.sealed {
                    if let Some(final_len) = state.final_len {
                        if range.start >= final_len {
                            return Ok(WaitOutcome::Eof);
                        }
                        // If the range extends beyond EOF, we only need the part before EOF.
                        let needed_end = range.end.min(final_len);
                        if state.is_covered(range.start..needed_end) {
                            return Ok(WaitOutcome::Ready);
                        }
                    } else {
                        // Committed but unknown final len: treat like infinite, still must be covered.
                        if state.is_covered(range.clone()) {
                            return Ok(WaitOutcome::Ready);
                        }
                    }
                } else {
                    if state.is_covered(range.clone()) {
                        return Ok(WaitOutcome::Ready);
                    }
                }
            }

            // Wait for either a writer update or cancellation.
            tokio::select! {
                _ = self.inner.cancel.cancelled() => return Err(StorageError::Cancelled),
                _ = self.inner.notify.notified() => { /* loop and re-check */ }
            }
        }
    }

    /// Commit the resource as "ready".
    ///
    /// - After commit, `write_at` is rejected.
    /// - `wait_range` can resolve `Eof` when `final_len` is known.
    pub async fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        {
            let mut state = self.inner.state.lock().await;
            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }
            state.sealed = true;
            state.final_len = final_len;
        }
        self.inner.notify.notify_waiters();
        Ok(())
    }

    /// Mark the resource as failed, waking all waiters.
    pub async fn fail(&self, error: impl Into<String>) {
        {
            let mut state = self.inner.state.lock().await;
            state.failed = Some(error.into());
        }
        self.inner.notify.notify_waiters();
    }

    async fn clamp_len_to_eof(&self, offset: u64, len: usize) -> StorageResult<Option<usize>> {
        let state = self.inner.state.lock().await;
        if !state.sealed {
            return Ok(None);
        }
        let Some(final_len) = state.final_len else {
            return Ok(None);
        };

        if offset >= final_len {
            return Ok(Some(0));
        }

        let max = (final_len - offset) as usize;
        Ok(Some(len.min(max)))
    }

    async fn read_exact_len(&self, offset: u64, len: usize) -> StorageResult<Bytes> {
        let mut disk = self.inner.disk.lock().await;
        let len: u64 = len.try_into().map_err(|_| StorageError::InvalidRange {
            start: offset,
            end: offset,
        })?;
        let data = disk.read(offset, len).await?;
        Ok(Bytes::from(data))
    }
}

struct Inner {
    path: PathBuf,
    cancel: CancellationToken,
    disk: Mutex<RandomAccessDisk>,
    state: Mutex<State>,
    notify: Notify,
}

#[derive(Debug)]
struct State {
    available: RangeSet<u64>,
    sealed: bool,
    final_len: Option<u64>,
    failed: Option<String>,

    // Internal: temporary stash used to only update availability after successful disk write.
    pending_add: Option<Range<u64>>,
}

impl State {
    fn new() -> Self {
        Self {
            available: RangeSet::new(),
            sealed: false,
            final_len: None,
            failed: None,
            pending_add: None,
        }
    }

    fn is_covered(&self, range: Range<u64>) -> bool {
        // `RangeSet` doesn't expose "covers fully" directly, so we check that there are no holes:
        // iterate overlapping fragments and ensure we can walk from start to end.
        //
        // This is intentionally implemented without cleverness; correctness first.
        let mut cursor = range.start;

        for r in self.available.overlapping(&(range.start..range.end)) {
            if r.start > cursor {
                return false; // gap
            }
            if r.end > cursor {
                cursor = r.end;
                if cursor >= range.end {
                    return true;
                }
            }
        }

        cursor >= range.end
    }
}

fn validate_range(range: &Range<u64>) -> StorageResult<()> {
    if range.start >= range.end {
        return Err(StorageError::InvalidRange {
            start: range.start,
            end: range.end,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests will be rewritten once higher-level assets/cache integration is in place.
    // Keeping this module here to reserve space for future behavior-driven tests.

    #[test]
    #[ignore = "placeholder; real tests will be added once the new API is wired into assets + io"]
    fn placeholder() {
        // Intentionally empty.
    }
}
