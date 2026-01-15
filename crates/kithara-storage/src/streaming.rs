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
use tokio::sync::{Mutex, Notify, RwLock};
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

    /// Optional initial length hint.
    ///
    /// - `Some(len)`: best-effort truncate/extend to `len` before writes.
    /// - `None`: if the file already exists, its current length is treated as available data
    ///   (resource remains in-progress); nothing is truncated.
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
/// - `commit` publishes an optional EOF hint; writes after commit are allowed.
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
    /// - If `initial_len` is `Some`, the file is truncated/extended accordingly.
    /// - If `initial_len` is `None` and the file exists, existing bytes are published as available
    ///   (resource stays in-progress) and nothing is truncated.
    ///
    /// This does not imply the resource is ready. Callers should write ranges and then `commit`.
    pub async fn open_disk(opts: DiskOptions) -> StorageResult<Self> {
        // Storage-layer responsibility: ensure parent directories exist.
        if let Some(parent) = opts.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let existing_len = tokio::fs::metadata(&opts.path).await.ok().map(|m| m.len());

        let mut disk = RandomAccessDisk::open(opts.path.clone()).await?;
        if let Some(len) = opts.initial_len {
            // Best-effort sizing. Some backends may create sparse files; that's fine.
            disk.truncate(len).await?;
        }

        let state = match (opts.initial_len, existing_len) {
            (Some(len), _) => State::with_initial_len(Some(len)),
            (None, Some(len)) if len > 0 => State::with_available_len(len),
            _ => State::new(),
        };

        Ok(Self {
            inner: Arc::new(Inner {
                path: opts.path,
                cancel: opts.cancel,
                disk: Mutex::new(disk),
                state: RwLock::new(state),
                notify: Notify::new(),
            }),
        })
    }

    /// Return current status (best-effort snapshot).
    pub async fn status(&self) -> ResourceStatus {
        let state = self.inner.state.read().await;

        if let Some(_e) = &state.failed {
            return ResourceStatus::Failed;
        }

        if state.committed {
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
        let state = self.inner.state.read().await;
        if !state.committed {
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
        let final_len = match self.inner.state.try_read() {
            Err(err) => {
                return Err(StorageError::Failed(format!(
                    "Failed to acquire lock {:?}",
                    err
                )));
            }
            Ok(state) => {
                if let Some(err) = &state.failed {
                    return Err(StorageError::Failed(err.clone()));
                }
                if !state.committed {
                    return Err(StorageError::NotCommitted);
                }
                match state.final_len {
                    Some(len) => len,
                    None => return Err(StorageError::NotCommitted),
                }
            }
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
        let mut buf = vec![0u8; len_usize];
        let bytes_read = self.read_at(0, &mut buf).await?;
        buf.truncate(bytes_read);
        Ok(Bytes::from(buf))
    }

    async fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        {
            let mut state = self.inner.state.write().await;

            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }

            state.committed = true;
            state.final_len = final_len;
        }

        self.inner.notify.notify_waiters();
        Ok(())
    }

    async fn fail(&self, error: impl Into<String> + Send) -> StorageResult<()> {
        {
            let mut state = self.inner.state.write().await;
            state.failed = Some(error.into());
        }
        self.inner.notify.notify_waiters();
        Ok(())
    }

    fn path(&self) -> &Path {
        &self.inner.path
    }
}

#[async_trait]
impl StreamingResourceExt for StreamingResource {
    async fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        Self::validate_range(&range)?;

        // Important: do not hold `state` across `.await` points.
        // Otherwise `wait_range` can block writers from publishing new ranges,
        // and writers can block `wait_range` from being notified (deadlock).
        loop {
            let outcome = {
                let state = self.inner.state.read().await;

                if let Some(err) = &state.failed {
                    return Err(StorageError::Failed(err.clone()));
                }

                let start = range.start;
                let end = range.end;

                match state.final_len {
                    Some(final_len) => {
                        if start >= final_len {
                            Some(WaitOutcome::Eof)
                        } else {
                            // If the range extends beyond EOF, only the part before EOF matters.
                            let needed_end = end.min(final_len);
                            if state.is_covered(start..needed_end) {
                                Some(WaitOutcome::Ready)
                            } else {
                                None
                            }
                        }
                    }
                    None => {
                        // No known final len: still must be covered.
                        if state.is_covered(start..end) {
                            Some(WaitOutcome::Ready)
                        } else {
                            None
                        }
                    }
                }
            };

            if let Some(outcome) = outcome {
                return Ok(outcome);
            }

            tokio::select! {
                _ = self.inner.cancel.cancelled() => return Err(StorageError::Cancelled),
                _ = self.inner.notify.notified() => { /* loop */ }
            }
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let len = buf.len();
        let read_len = match self.clamp_len_to_eof(offset, len).await? {
            Some(0) => return Ok(0),
            Some(clamped) => clamped,
            None => len,
        };

        let mut disk = self.inner.disk.lock().await;
        let bytes_read = disk.read_to(offset, &mut buf[..read_len]).await?;
        Ok(bytes_read)
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }

        if data.is_empty() {
            return Ok(());
        }

        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(StorageError::InvalidRange {
                start: offset,
                end: offset,
            })?;

        // Stash range locally to only publish it after successful disk write.
        let pending_range = offset..end;

        // Check preconditions before disk write
        {
            let state = self.inner.state.read().await;

            if let Some(err) = &state.failed {
                return Err(StorageError::Failed(err.clone()));
            }
        }

        // Perform disk write
        let write_result: StorageResult<()> = {
            let mut disk = self.inner.disk.lock().await;
            disk.write(offset, data).await.map_err(Into::into)
        };

        write_result?;

        // Add the range to available after successful disk write
        {
            let mut state = self.inner.state.write().await;
            if state.committed {
                state.committed = false;
                state.final_len = None;
            }
            state.available.insert(pending_range);
        }

        self.inner.notify.notify_waiters();
        Ok(())
    }
}

struct Inner {
    path: PathBuf,
    cancel: CancellationToken,
    disk: Mutex<RandomAccessDisk>,
    state: RwLock<State>,
    notify: Notify,
}

#[derive(Debug)]
struct State {
    available: RangeSet<u64>,
    committed: bool,
    final_len: Option<u64>,
    failed: Option<String>,
    // Temporary range published only after successful write.
}

impl State {
    fn new() -> Self {
        Self {
            available: RangeSet::new(),
            committed: false,
            final_len: None,
            failed: None,
        }
    }

    fn with_initial_len(initial_len: Option<u64>) -> Self {
        match initial_len {
            Some(len) if len > 0 => {
                let mut available = RangeSet::new();
                available.insert(0..len);
                Self {
                    available,
                    committed: true,
                    final_len: Some(len),
                    failed: None,
                }
            }
            _ => Self::new(),
        }
    }

    fn with_available_len(len: u64) -> Self {
        if len == 0 {
            return Self::new();
        }

        let mut available = RangeSet::new();
        available.insert(0..len);

        Self {
            available,
            committed: false,
            final_len: None,
            failed: None,
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
