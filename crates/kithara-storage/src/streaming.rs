#![forbid(unsafe_code)]

use std::{
    fmt,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use random_access_disk::RandomAccessDisk;
use random_access_storage::RandomAccess;
use rangemap::RangeSet;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::{Resource, StorageError, StorageResult, StreamingResourceExt, WaitOutcome};

/// Options for opening a disk-backed streaming resource.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceStatus {
    InProgress,
    Committed { final_len: Option<u64> },
    Failed,
}

/// A single logical resource that can be written in ranges and read with async waiting.
///
/// Clone is cheap; all clones refer to the same underlying resource.
///
/// # Contract (normative)
/// - `read_at` does **not** wait; callers should use `wait_range` for blocking semantics.
/// - `wait_range` resolves to `Eof` only after `commit(Some(final_len))` when the requested
///   range starts at/after EOF.
/// - `commit` seals the resource; subsequent writes fail with `Sealed`.
/// - `fail` wakes all waiters and causes future waits/writes to fail.
#[derive(Clone)]
pub struct StreamingResource {
    inner: Arc<Inner>,
}

impl fmt::Debug for StreamingResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamingResource")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl StreamingResource {
    /// Open or create a disk-backed streaming resource.
    ///
    /// This does not imply the resource is ready. Callers should write ranges and then `commit`.
    pub async fn open_disk(opts: DiskOptions) -> StorageResult<Self> {
        // Storage-layer responsibility: ensure parent directories exist.
        if let Some(parent) = opts.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

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

        if let Some(_e) = &state.failed {
            return ResourceStatus::Failed;
        }

        if state.sealed {
            return ResourceStatus::Committed {
                final_len: state.final_len,
            };
        }

        ResourceStatus::InProgress
    }

    /// Clamp `len` to EOF if committed with known final length.
    ///
    /// Returns:
    /// - `Ok(Some(0))` if offset is at/after EOF.
    /// - `Ok(Some(clamped_len))` if clamped.
    /// - `Ok(None)` if EOF is not known (not committed or committed without final_len).
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

    fn validate_range(range: &Range<u64>) -> StorageResult<()> {
        if range.start >= range.end {
            return Err(StorageError::InvalidRange {
                start: range.start,
                end: range.end,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Resource for StreamingResource {
    async fn write(&self, data: &[u8]) -> StorageResult<()> {
        // Whole-object convenience for streaming resources:
        // write at offset 0, then commit with known final_len.
        self.write_at(0, data).await?;
        self.commit(Some(data.len() as u64)).await?;
        Ok(())
    }

    async fn read(&self) -> StorageResult<Bytes> {
        // Whole-object reads are only well-defined once committed with a known final_len.
        let final_len = {
            let state = self.inner.state.lock().await;
            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }
            if !state.sealed {
                return Err(StorageError::Sealed);
            }
            state.final_len
        };

        let Some(final_len) = final_len else {
            return Err(StorageError::Sealed);
        };

        if final_len == 0 {
            return Ok(Bytes::new());
        }

        self.wait_range(0..final_len).await?;
        let len_usize: usize = final_len
            .try_into()
            .map_err(|_| StorageError::InvalidRange {
                start: 0,
                end: final_len,
            })?;
        self.read_at(0, len_usize).await
    }

    async fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
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

    async fn fail(&self, error: impl Into<String> + Send) -> StorageResult<()> {
        {
            let mut state = self.inner.state.lock().await;
            state.failed = Some(error.into());
        }
        self.inner.notify.notify_waiters();
        Ok(())
    }
}

#[async_trait]
impl StreamingResourceExt for StreamingResource {
    async fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        Self::validate_range(&range)?;

        loop {
            let state = self.inner.state.lock().await;

            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }

            if !state.sealed && state.is_covered(range.clone()) {
                return Ok(WaitOutcome::Ready);
            }

            if let Some(final_len) = state.final_len {
                if range.start >= final_len {
                    return Ok(WaitOutcome::Eof);
                }

                // If the range extends beyond EOF, only the part before EOF matters.
                let needed_end = range.end.min(final_len);
                if state.is_covered(range.start..needed_end) {
                    return Ok(WaitOutcome::Ready);
                }
            } else {
                // Committed but unknown final len: treat as "infinite", still must be covered.
                if state.is_covered(range.clone()) {
                    return Ok(WaitOutcome::Ready);
                }
            }

            tokio::select! {
                _ = self.inner.cancel.cancelled() => return Err(StorageError::Cancelled),
                _ = self.inner.notify.notified() => { /* loop */ }
            }
        }
    }

    async fn read_at(&self, offset: u64, len: usize) -> StorageResult<Bytes> {
        if len == 0 {
            return Ok(Bytes::new());
        }

        if let Some(clamped) = self.clamp_len_to_eof(offset, len).await? {
            if clamped == 0 {
                return Ok(Bytes::new());
            }
            return self.read_exact_len(offset, clamped).await;
        }

        self.read_exact_len(offset, len).await
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
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

            // Stash range to only publish it after successful disk write.
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

    // Temporary range published only after successful write.
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
        // Check that there are no holes between range.start and range.end,
        // using the set's overlapping iterator.
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

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "tests will be written once atomic resource + manager integration is complete"]
    fn placeholder() {}
}
