#![forbid(unsafe_code)]

//! Mmap-backed storage driver.
//!
//! [`MmapDriver`] implements [`Driver`](crate::Driver) backed by `mmap-io`
//! with lock-free queues for fast-path notifications.
//!
//! ## Performance optimizations
//!
//! - **Lock-free fast path**: Writer publishes ready ranges to `SegQueue`.
//!   Reader checks queue before blocking on mutex, reducing contention.
//! - **Fallback slow path**: If queue is empty, falls back to `Mutex + Condvar`
//!   for full synchronization.

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use crossbeam_queue::SegQueue;
use kithara_platform::Mutex;
use mmap_io::MemoryMappedFile;

use crate::{
    StorageError, StorageResult,
    driver::{Driver, DriverState, Resource},
    resource::OpenMode,
};

/// Default initial size for new mmap files (64 KB).
const DEFAULT_INITIAL_SIZE: u64 = 64 * 1024;

/// Options for opening a [`MmapResource`].
#[derive(Debug, Clone)]
pub struct MmapOptions {
    /// Path to the backing file.
    pub path: PathBuf,
    /// Initial file size for new files. Ignored for existing files.
    pub initial_len: Option<u64>,
    /// Open mode controlling read/write behavior for existing files.
    pub mode: OpenMode,
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

/// Mmap-backed storage driver.
///
/// Uses `mmap-io` for file-backed storage with a lock-free `SegQueue`
/// for fast-path wait notifications.
pub struct MmapDriver {
    mmap: Mutex<MmapState>,
    /// Lock-free queue for fast-path range notifications.
    ready_ranges: SegQueue<Range<u64>>,
    path: PathBuf,
    mode: OpenMode,
}

impl std::fmt::Debug for MmapDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapDriver")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl Driver for MmapDriver {
    type Options = MmapOptions;

    fn open(opts: MmapOptions) -> StorageResult<(Self, DriverState)> {
        let mode = opts.mode;

        let (mmap_state, init) = if opts.path.exists() && std::fs::metadata(&opts.path)?.len() > 0 {
            let len;
            let mmap_state = if mode == OpenMode::ReadWrite {
                let mmap = MemoryMappedFile::open_rw(&opts.path)?;
                len = mmap.len();
                MmapState::Active(mmap)
            } else {
                let mmap = MemoryMappedFile::open_ro(&opts.path)?;
                len = mmap.len();
                MmapState::Committed(mmap)
            };
            let mut available = rangemap::RangeSet::new();
            available.insert(0..len);
            let init = DriverState {
                available,
                committed: true,
                final_len: Some(len),
            };
            (mmap_state, init)
        } else if mode == OpenMode::ReadOnly {
            (
                MmapState::Empty,
                DriverState {
                    available: rangemap::RangeSet::new(),
                    committed: true,
                    final_len: Some(0),
                },
            )
        } else {
            if let Some(parent) = opts.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let size = opts.initial_len.unwrap_or(DEFAULT_INITIAL_SIZE);
            let mmap_state = if size == 0 {
                MmapState::Empty
            } else {
                let mmap = MemoryMappedFile::create_rw(&opts.path, size)?;
                MmapState::Active(mmap)
            };
            (mmap_state, DriverState::default())
        };

        let driver = Self {
            mmap: Mutex::new(mmap_state),
            ready_ranges: SegQueue::new(),
            path: opts.path,
            mode,
        };

        Ok((driver, init))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        {
            let mmap_guard = self.mmap.lock();
            if let Some(mmap) = mmap_guard.as_readable() {
                mmap.read_into(offset, buf)?;
            }
        }
        Ok(buf.len())
    }

    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        let end = offset + data.len() as u64;
        let mut mmap_guard = self.mmap.lock();

        // Handle writes when common state is committed.
        //
        // The `committed` flag is a hint from common state — the driver decides
        // whether the underlying storage can accept writes.
        //
        // - `Committed` + `ReadWrite`: reopen as rw (index files rewritten in place).
        // - `Active` + any mode: already writable, allow the write.
        // - `Empty` + non-ReadOnly: no backing data to protect, fall through to
        //   creation logic below (handles zero-length commit → resume write).
        // - Otherwise: reject (use `reactivate()` to resume writing).
        if committed {
            match (&*mmap_guard, self.mode) {
                (MmapState::Committed(_), OpenMode::ReadWrite) => {
                    *mmap_guard = MmapState::Empty;
                    let rw = MemoryMappedFile::open_rw(&self.path)?;
                    *mmap_guard = MmapState::Active(rw);
                }
                (MmapState::Active(_), _)
                | (MmapState::Empty, OpenMode::Auto | OpenMode::ReadWrite) => {
                    // Already active or zero-length committed — ok to proceed.
                }
                _ => {
                    return Err(StorageError::Failed(
                        "cannot write to committed resource".to_string(),
                    ));
                }
            }
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
                let m = MemoryMappedFile::create_rw(&self.path, size)?;
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
        Ok(())
    }

    #[expect(clippy::significant_drop_tightening)] // lock guards mmap state transitions throughout
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let mut mmap_guard = self.mmap.lock();

        if let Some(len) = final_len {
            if len > 0 {
                // Check if truncation is needed before dropping the mmap.
                let needs_truncate = matches!(
                    &*mmap_guard,
                    MmapState::Active(mmap) if len < mmap.len()
                );

                // Drop old mmap first — avoids SIGBUS from resizing a stale
                // mmap (e.g. after atomic write-rename replaces the backing file).
                *mmap_guard = MmapState::Empty;

                if needs_truncate {
                    // Truncate file to final size via ftruncate (no mmap involved).
                    // After atomic rename, the file already has the correct size
                    // so this branch is skipped.
                    let file_len = std::fs::metadata(&self.path).map_or(0, |m| m.len());
                    if file_len > len {
                        let f = std::fs::OpenOptions::new()
                            .write(true)
                            .open(&self.path)
                            .map_err(|e| StorageError::Failed(format!("truncate open: {e}")))?;
                        f.set_len(len)
                            .map_err(|e| StorageError::Failed(format!("truncate: {e}")))?;
                    }
                }

                let ro = MemoryMappedFile::open_ro(&self.path)?;
                *mmap_guard = MmapState::Committed(ro);
            } else {
                *mmap_guard = MmapState::Empty;
                let _ = std::fs::write(&self.path, b"");
            }
        } else {
            let is_active = matches!(*mmap_guard, MmapState::Active(_));
            if is_active {
                *mmap_guard = MmapState::Empty;
                if self.path.exists() && std::fs::metadata(&self.path).is_ok_and(|m| m.len() > 0) {
                    let ro = MemoryMappedFile::open_ro(&self.path)?;
                    *mmap_guard = MmapState::Committed(ro);
                }
            }
        }

        Ok(())
    }

    #[expect(clippy::significant_drop_tightening)] // lock guards mmap state transition
    fn reactivate(&self) -> StorageResult<()> {
        let mut mmap_guard = self.mmap.lock();

        match &*mmap_guard {
            MmapState::Active(_) => {}
            MmapState::Committed(_) | MmapState::Empty => {
                *mmap_guard = MmapState::Empty;
                if self.path.exists() && std::fs::metadata(&self.path).is_ok_and(|m| m.len() > 0) {
                    let rw = MemoryMappedFile::open_rw(&self.path)?;
                    *mmap_guard = MmapState::Active(rw);
                }
            }
        }

        Ok(())
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn storage_len(&self) -> u64 {
        let mmap_guard = self.mmap.lock();
        mmap_guard.len()
    }

    fn try_fast_check(&self, range: &Range<u64>) -> bool {
        let mut found_match = false;
        while let Some(ready) = self.ready_ranges.pop() {
            if ready.start <= range.start && ready.end >= range.end {
                found_match = true;
                break;
            }
        }
        found_match
    }

    fn notify_write(&self, range: &Range<u64>) {
        self.ready_ranges.push(range.clone());
    }
}

/// Mmap-backed storage resource.
///
/// Type alias for [`Resource<MmapDriver>`].
pub type MmapResource = Resource<MmapDriver>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::*;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        StorageError,
        resource::{ResourceExt, ResourceStatus, WaitOutcome},
    };

    fn create_resource(dir: &TempDir) -> MmapResource {
        create_resource_with_size(dir, None)
    }

    fn create_resource_with_size(dir: &TempDir, size: Option<u64>) -> MmapResource {
        let path = dir.path().join("test.dat");
        Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: size,
                mode: OpenMode::Auto,
            },
        )
        .expect("open test resource")
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

        let res: MmapResource = Resource::open(
            cancel.clone(),
            MmapOptions {
                path,
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .expect("open cancel test resource");

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
            let res: MmapResource = Resource::open(
                CancellationToken::new(),
                MmapOptions {
                    path: path.clone(),
                    initial_len: None,
                    mode: OpenMode::Auto,
                },
            )
            .expect("open first resource");
            res.write_all(b"persisted data").unwrap();
        }

        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: None,
                mode: OpenMode::Auto,
            },
        )
        .expect("open reopened resource");

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
