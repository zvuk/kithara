#![forbid(unsafe_code)]

//! Unified storage resource: trait + mmap-backed implementation.
//!
//! `ResourceExt` is a sync trait covering both streaming (incremental write) and
//! atomic (whole-file) use-cases. `StorageResource` is the sole implementation,
//! backed by `mmap-io` with lock-free queues for fast-path notifications and
//! `Condvar` for blocking waits.
//!
//! ## Performance optimizations
//!
//! - **Lock-free fast path**: Writer publishes ready ranges to `SegQueue`.
//!   Reader checks queue before blocking on mutex, reducing contention.
//! - **Fallback slow path**: If queue is empty, falls back to `Mutex + Condvar`
//!   for full synchronization.
//! - **Hybrid approach**: Combines lock-free performance with mutex correctness.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use crossbeam_queue::SegQueue;
use mmap_io::MemoryMappedFile;
use parking_lot::{Condvar, Mutex};
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;
use tracing::trace;

use crate::{StorageError, StorageResult};

/// Controls how `StorageResource` opens the backing file.
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
    /// Transitions Committed → Active: reopens the backing file as read-write,
    /// resets committed flag, and clears `final_len`. Existing data remains
    /// available for reading. New data can be written at any offset.
    ///
    /// Use for resuming partial downloads where the file on disk is smaller
    /// than the expected total.
    ///
    /// No-op if the resource is already Active.
    fn reactivate(&self) -> StorageResult<()>;

    // ---- Convenience methods (atomic-style) ----

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

/// Default initial size for new mmap files (64 KB).
const DEFAULT_INITIAL_SIZE: u64 = 64 * 1024;

/// Options for opening a `StorageResource`.
#[derive(Debug, Clone)]
pub struct StorageOptions {
    /// Path to the backing file.
    pub path: PathBuf,
    /// Initial file size for new files. Ignored for existing files.
    pub initial_len: Option<u64>,
    /// Open mode controlling read/write behavior for existing files.
    pub mode: OpenMode,
    /// Cancellation token.
    pub cancel: CancellationToken,
}

/// Internal state protected by Mutex.
struct State {
    available: RangeSet<u64>,
    committed: bool,
    final_len: Option<u64>,
    failed: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            available: RangeSet::new(),
            committed: false,
            final_len: None,
            failed: None,
        }
    }
}

/// Mmap state machine.
///
/// - `Active`: read-write mmap, used during streaming/writing.
/// - `Committed`: read-only mmap, after commit (no writes allowed).
/// - `Empty`: zero-length committed resource (no mmap needed).
enum MmapState {
    Active(MemoryMappedFile),
    Committed(MemoryMappedFile),
    Empty,
}

impl MmapState {
    fn as_readable(&self) -> Option<&MemoryMappedFile> {
        match self {
            Self::Active(m) | Self::Committed(m) => Some(m),
            Self::Empty => None,
        }
    }

    fn len(&self) -> u64 {
        match self {
            Self::Active(m) | Self::Committed(m) => m.len(),
            Self::Empty => 0,
        }
    }
}

/// Shared inner storage.
struct Inner {
    mmap: Mutex<MmapState>,
    state: Mutex<State>,
    condvar: Condvar,
    /// Lock-free queue for fast-path range notifications.
    /// Writer pushes ready ranges here; Reader checks queue before blocking.
    ready_ranges: SegQueue<Range<u64>>,
    cancel: CancellationToken,
    path: PathBuf,
    mode: OpenMode,
}

/// Unified mmap-backed storage resource.
///
/// Supports both streaming (incremental `write_at` -> `wait_range` + `read_at` -> `commit`)
/// and atomic (`write_all` / `read_into`) use-cases.
#[derive(Clone)]
pub struct StorageResource {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for StorageResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageResource")
            .field("path", &self.inner.path)
            .finish()
    }
}

impl StorageResource {
    /// Open or create a storage resource.
    ///
    /// Behavior depends on [`OpenMode`]:
    /// - `Auto`: existing files → committed (read-only), new files → active (read-write).
    /// - `ReadWrite`: existing files → active (read-write, no truncation), new files → active.
    /// - `ReadOnly`: existing files → committed (read-only), missing files → empty committed.
    pub fn open(opts: StorageOptions) -> StorageResult<Self> {
        let mode = opts.mode;

        let (mmap_state, state) = if opts.path.exists() && std::fs::metadata(&opts.path)?.len() > 0
        {
            let len;
            let mmap_state = if mode == OpenMode::ReadWrite {
                // ReadWrite: open existing file as read-write (no truncation).
                let mmap = MemoryMappedFile::open_rw(&opts.path)?;
                len = mmap.len();
                MmapState::Active(mmap)
            } else {
                // Auto / ReadOnly: open existing file as read-only.
                let mmap = MemoryMappedFile::open_ro(&opts.path)?;
                len = mmap.len();
                MmapState::Committed(mmap)
            };
            let mut available = RangeSet::new();
            available.insert(0..len);
            let state = State {
                available,
                committed: true,
                final_len: Some(len),
                failed: None,
            };
            (mmap_state, state)
        } else if mode == OpenMode::ReadOnly {
            // ReadOnly on missing/empty file: empty committed resource.
            (
                MmapState::Empty,
                State {
                    available: RangeSet::new(),
                    committed: true,
                    final_len: Some(0),
                    failed: None,
                },
            )
        } else {
            // Auto / ReadWrite: create new file.
            if let Some(parent) = opts.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let size = opts.initial_len.unwrap_or(DEFAULT_INITIAL_SIZE);
            if size == 0 {
                (MmapState::Empty, State::default())
            } else {
                let mmap = MemoryMappedFile::create_rw(&opts.path, size)?;
                (MmapState::Active(mmap), State::default())
            }
        };

        Ok(Self {
            inner: Arc::new(Inner {
                mmap: Mutex::new(mmap_state),
                state: Mutex::new(state),
                condvar: Condvar::new(),
                ready_ranges: SegQueue::new(),
                cancel: opts.cancel,
                path: opts.path,
                mode,
            }),
        })
    }

    /// Check if the resource is cancelled or failed, returning error if so.
    fn check_health(&self) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }
        let state = self.inner.state.lock();
        if let Some(ref reason) = state.failed {
            return Err(StorageError::Failed(reason.clone()));
        }
        Ok(())
    }
}

impl ResourceExt for StorageResource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.check_health()?;

        let state = self.inner.state.lock();
        let mmap_guard = self.inner.mmap.lock();

        let mmap_len = mmap_guard.len();
        let final_len = state.final_len.unwrap_or(mmap_len);

        // Clamp to both final_len and actual mmap size
        let effective_len = final_len.min(mmap_len);

        if offset >= effective_len {
            return Ok(0);
        }

        let available = (effective_len - offset) as usize;
        let to_read = buf.len().min(available);
        drop(state);

        if let Some(mmap) = mmap_guard.as_readable() {
            mmap.read_into(offset, &mut buf[..to_read])?;
        }
        drop(mmap_guard);

        Ok(to_read)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        if data.is_empty() {
            return Ok(());
        }

        self.check_health()?;

        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(StorageError::InvalidRange {
                start: offset,
                end: u64::MAX,
            })?;

        {
            let mut mmap_guard = self.inner.mmap.lock();

            // ReadWrite mode: transition Committed → Active via open_rw.
            if self.inner.mode == OpenMode::ReadWrite
                && matches!(&*mmap_guard, MmapState::Committed(_))
            {
                *mmap_guard = MmapState::Empty;
                let rw = MemoryMappedFile::open_rw(&self.inner.path)?;
                *mmap_guard = MmapState::Active(rw);
            }

            let mmap = match &*mmap_guard {
                MmapState::Active(m) => {
                    if end > m.len() {
                        let new_size = end.max(m.len() * 2);
                        m.resize(new_size)?;
                    }
                    m
                }
                MmapState::Committed(_) => {
                    return Err(StorageError::Failed(
                        "cannot write to committed resource".to_string(),
                    ));
                }
                MmapState::Empty => {
                    let size = end.max(DEFAULT_INITIAL_SIZE);
                    let m = MemoryMappedFile::create_rw(&self.inner.path, size)?;
                    *mmap_guard = MmapState::Active(m);
                    match &*mmap_guard {
                        MmapState::Active(m) => m,
                        _ => {
                            return Err(StorageError::Failed(
                                "mmap not available after create".to_string(),
                            ));
                        }
                    }
                }
            };

            mmap.update_region(offset, data)?;
        }

        // Lock-free fast path: publish ready range to queue.
        // Readers check this queue before blocking on condvar.
        let range = offset..end;
        self.inner.ready_ranges.push(range.clone());

        // Slow path: update shared state and notify waiters.
        let mut state = self.inner.state.lock();
        state.available.insert(range);
        self.inner.condvar.notify_all();

        trace!(offset, len = data.len(), "StorageResource: write_at");
        Ok(())
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        if range.start > range.end {
            return Err(StorageError::InvalidRange {
                start: range.start,
                end: range.end,
            });
        }

        if range.is_empty() {
            return Ok(WaitOutcome::Ready);
        }

        // Fast path: consume lock-free queue and check coverage without blocking.
        // This avoids mutex contention when writer is actively publishing ranges.
        loop {
            // Drain all ready ranges from queue and check if our range is covered.
            let mut found_match = false;
            while let Some(ready) = self.inner.ready_ranges.pop() {
                // Check if this ready range overlaps or covers our requested range.
                if ready.start <= range.start && ready.end >= range.end {
                    found_match = true;
                    break;
                }
                // Even if not a match, the range was already inserted into state.available
                // by write_at, so we don't need to re-insert it here.
            }

            if found_match {
                return Ok(WaitOutcome::Ready);
            }

            // Slow path: lock state and check coverage, wait if needed.
            let mut state = self.inner.state.lock();

            // Check cancellation and failure.
            if self.inner.cancel.is_cancelled() {
                return Err(StorageError::Cancelled);
            }

            if let Some(ref reason) = state.failed {
                return Err(StorageError::Failed(reason.clone()));
            }

            // Check if range is now available (might have been added between queue check and lock).
            if range_covered_by(&state.available, &range) {
                return Ok(WaitOutcome::Ready);
            }

            // Check if committed and EOF.
            if state.committed {
                let final_len = state.final_len.unwrap_or(0);
                if range.start >= final_len {
                    return Ok(WaitOutcome::Eof);
                }
                return Ok(WaitOutcome::Ready);
            }

            // Wait for notification (with timeout to periodically re-check queue).
            self.inner
                .condvar
                .wait_for(&mut state, std::time::Duration::from_millis(50));

            // Drop lock and re-check queue in next iteration.
        }
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;

        {
            let mut mmap_guard = self.inner.mmap.lock();

            if let Some(len) = final_len {
                if len > 0 {
                    if let MmapState::Active(ref mmap) = *mmap_guard
                        && len < mmap.len()
                    {
                        mmap.resize(len)?;
                    }
                    *mmap_guard = MmapState::Empty;
                    let ro = MemoryMappedFile::open_ro(&self.inner.path)?;
                    *mmap_guard = MmapState::Committed(ro);
                } else {
                    *mmap_guard = MmapState::Empty;
                    let _ = std::fs::write(&self.inner.path, b"");
                }
            } else {
                let is_active = matches!(*mmap_guard, MmapState::Active(_));
                if is_active {
                    *mmap_guard = MmapState::Empty;
                    if self.inner.path.exists()
                        && std::fs::metadata(&self.inner.path)
                            .map(|m| m.len() > 0)
                            .unwrap_or(false)
                    {
                        let ro = MemoryMappedFile::open_ro(&self.inner.path)?;
                        *mmap_guard = MmapState::Committed(ro);
                    }
                }
            }
        }

        let mut state = self.inner.state.lock();
        state.committed = true;
        state.final_len = final_len;
        if let Some(len) = final_len
            && len > 0
        {
            state.available.insert(0..len);
        }
        self.inner.condvar.notify_all();
        Ok(())
    }

    fn fail(&self, reason: String) {
        let mut state = self.inner.state.lock();
        state.failed = Some(reason);
        self.inner.condvar.notify_all();
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.inner.path)
    }

    fn len(&self) -> Option<u64> {
        let state = self.inner.state.lock();
        state.final_len
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.check_health()?;

        {
            let mut mmap_guard = self.inner.mmap.lock();

            match &*mmap_guard {
                MmapState::Active(_) => {
                    // Already active, nothing to do for mmap.
                }
                MmapState::Committed(_) | MmapState::Empty => {
                    // Reopen as read-write.
                    *mmap_guard = MmapState::Empty;
                    if self.inner.path.exists()
                        && std::fs::metadata(&self.inner.path)
                            .map(|m| m.len() > 0)
                            .unwrap_or(false)
                    {
                        let rw = MemoryMappedFile::open_rw(&self.inner.path)?;
                        *mmap_guard = MmapState::Active(rw);
                    }
                }
            }
        }

        let mut state = self.inner.state.lock();
        state.committed = false;
        state.final_len = None;
        // Keep available data as-is — existing bytes remain readable.
        self.inner.condvar.notify_all();
        Ok(())
    }

    fn status(&self) -> ResourceStatus {
        let state = self.inner.state.lock();
        if let Some(ref reason) = state.failed {
            ResourceStatus::Failed(reason.clone())
        } else if state.committed {
            ResourceStatus::Committed {
                final_len: state.final_len,
            }
        } else {
            ResourceStatus::Active
        }
    }
}

/// Check if the `available` range set fully covers `range`.
fn range_covered_by(available: &RangeSet<u64>, range: &Range<u64>) -> bool {
    if range.is_empty() {
        return true;
    }
    !available.gaps(range).any(|_| true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::*;
    use tempfile::TempDir;

    use super::*;

    fn create_resource(dir: &TempDir) -> StorageResource {
        create_resource_with_size(dir, None)
    }

    fn create_resource_with_size(dir: &TempDir, size: Option<u64>) -> StorageResource {
        let path = dir.path().join("test.dat");
        StorageResource::open(StorageOptions {
            path,
            initial_len: size,
            mode: OpenMode::Auto,
            cancel: CancellationToken::new(),
        })
        .unwrap()
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_create_new_resource() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        assert_eq!(res.len(), None);
        assert_eq!(res.status(), ResourceStatus::Active);
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_write_and_read() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_at(0, b"hello world").unwrap();
        res.commit(Some(11)).unwrap();

        let mut buf = [0u8; 11];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_write_all_read_into() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_all(b"atomic data").unwrap();

        let mut buf = Vec::new();
        let n = res.read_into(&mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf[..], b"atomic data");
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_wait_range_ready() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_at(0, b"data").unwrap();

        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn test_wait_range_blocks_then_ready() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        let res2 = res.clone();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            res2.write_at(0, b"delayed data").unwrap();
        });

        let outcome = res.wait_range(0..12).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
        handle.join().unwrap();
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_wait_range_eof() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_at(0, b"short").unwrap();
        res.commit(Some(5)).unwrap();

        let outcome = res.wait_range(5..10).unwrap();
        assert_eq!(outcome, WaitOutcome::Eof);
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_fail_wakes_waiters() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        let res2 = res.clone();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            res2.fail("test error".to_string());
        });

        let result = res.wait_range(0..100);
        assert!(result.is_err());
        handle.join().unwrap();
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn test_cancel_wakes_waiters() {
        let dir = TempDir::new().unwrap();
        let cancel = CancellationToken::new();
        let path = dir.path().join("cancel_test.dat");

        let res = StorageResource::open(StorageOptions {
            path,
            initial_len: None,
            mode: OpenMode::Auto,
            cancel: cancel.clone(),
        })
        .unwrap();

        let handle = std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                std::thread::sleep(Duration::from_millis(50));
                cancel.cancel();
            }
        });

        let result = res.wait_range(0..100);
        assert!(matches!(result, Err(StorageError::Cancelled)));
        handle.join().unwrap();
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_open_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("existing.dat");

        {
            let res = StorageResource::open(StorageOptions {
                path: path.clone(),
                initial_len: None,
                mode: OpenMode::Auto,
                cancel: CancellationToken::new(),
            })
            .unwrap();
            res.write_all(b"persisted data").unwrap();
        }

        let res = StorageResource::open(StorageOptions {
            path,
            initial_len: None,
            mode: OpenMode::Auto,
            cancel: CancellationToken::new(),
        })
        .unwrap();

        assert_eq!(
            res.status(),
            ResourceStatus::Committed {
                final_len: Some(14)
            }
        );
        let mut buf = Vec::new();
        res.read_into(&mut buf).unwrap();
        assert_eq!(&buf[..], b"persisted data");
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_resize_on_large_write() {
        let dir = TempDir::new().unwrap();
        let res = create_resource_with_size(&dir, Some(16));

        let big_data = vec![42u8; 1024];
        res.write_at(0, &big_data).unwrap();
        res.commit(Some(1024)).unwrap();

        let mut buf = vec![0u8; 1024];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 1024);
        assert!(buf.iter().all(|&b| b == 42));
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_status_transitions() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        assert_eq!(res.status(), ResourceStatus::Active);

        res.write_at(0, b"data").unwrap();
        assert_eq!(res.status(), ResourceStatus::Active);

        res.commit(Some(4)).unwrap();
        assert_eq!(
            res.status(),
            ResourceStatus::Committed { final_len: Some(4) }
        );
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_status_failed() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }
}
