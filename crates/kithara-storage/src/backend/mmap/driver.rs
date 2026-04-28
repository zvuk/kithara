#![forbid(unsafe_code)]

//! [`MmapDriver`] struct, options, mmap state machine, and the
//! [`Driver`] factory impl. [`MmapResource`] alias lives here.
//! The [`DriverIo`](crate::DriverIo) impl is in the sibling `io` module.

use std::{fmt, fs, ops::Range, path::PathBuf};

use crossbeam_queue::SegQueue;
use kithara_platform::Mutex;
use mmap_io::MemoryMappedFile;
use rangemap::RangeSet;

use crate::{
    StorageResult,
    backend::{
        resource::Resource,
        traits::{Driver, DriverState},
    },
    resource::OpenMode,
};

/// Options for opening a [`MmapResource`].
#[derive(Debug, Clone)]
pub struct MmapOptions {
    /// Open mode controlling read/write behavior for existing files.
    pub mode: OpenMode,
    /// Initial file size for new files. Ignored for existing files.
    pub initial_len: Option<u64>,
    /// Path to the backing file.
    pub path: PathBuf,
}

/// Mmap state machine.
///
/// - `Active`: read-write mmap, used during streaming/writing.
/// - `Committed`: read-only mmap, after commit (no writes allowed).
/// - `Empty`: zero-length committed resource (no mmap needed).
pub(super) enum MmapState {
    Active(MemoryMappedFile),
    Committed(MemoryMappedFile),
    Empty,
}

impl MmapState {
    pub(super) fn as_readable(&self) -> Option<&MemoryMappedFile> {
        match self {
            Self::Active(m) | Self::Committed(m) => Some(m),
            Self::Empty => None,
        }
    }

    pub(super) fn len(&self) -> u64 {
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
    pub(super) mmap: Mutex<MmapState>,
    pub(super) mode: OpenMode,
    pub(super) path: PathBuf,
    /// Lock-free queue for fast-path range notifications.
    pub(super) ready_ranges: SegQueue<Range<u64>>,
}

impl fmt::Debug for MmapDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MmapDriver")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

pub(super) struct Consts;
impl Consts {
    /// Default initial size for new mmap files (64 KB).
    pub(super) const DEFAULT_INITIAL_SIZE: u64 = 64 * 1024;

    /// Growth factor when the mmap file needs to be resized.
    pub(super) const MMAP_GROWTH_FACTOR: u64 = 2;
}

impl Driver for MmapDriver {
    type Options = MmapOptions;

    fn open(opts: MmapOptions) -> StorageResult<(Self, DriverState)> {
        let mode = opts.mode;

        let (mmap_state, init) = if opts.path.exists() && fs::metadata(&opts.path)?.len() > 0 {
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
            let mut available = RangeSet::new();
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
                    available: RangeSet::new(),
                    committed: true,
                    final_len: Some(0),
                },
            )
        } else {
            if let Some(parent) = opts.path.parent() {
                fs::create_dir_all(parent)?;
            }
            let size = opts.initial_len.unwrap_or(Consts::DEFAULT_INITIAL_SIZE);
            let mmap_state = if size == 0 {
                MmapState::Empty
            } else {
                let mmap = MemoryMappedFile::create_rw(&opts.path, size)?;
                MmapState::Active(mmap)
            };
            (mmap_state, DriverState::default())
        };

        let driver = Self {
            mode,
            mmap: Mutex::new(mmap_state),
            path: opts.path,
            ready_ranges: SegQueue::new(),
        };

        Ok((driver, init))
    }
}

/// Mmap-backed storage resource.
///
/// Type alias for [`Resource<MmapDriver>`].
pub type MmapResource = Resource<MmapDriver>;

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use kithara_platform::{thread, time::Duration};
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_create_new_resource() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        assert_eq!(res.len(), None);
        assert_eq!(res.status(), ResourceStatus::Active);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_write_all_read_into() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_all(b"atomic data").unwrap();

        let mut buf = Vec::new();
        let n = res.read_into(&mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf[..], b"atomic data");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_wait_range_ready() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_at(0, b"data").unwrap();

        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn test_wait_range_blocks_then_ready() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        let res2 = res.clone();

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            res2.write_at(0, b"delayed data").unwrap();
        });

        let outcome = res.wait_range(0..12).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
        handle.join().unwrap();
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_wait_range_eof() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_at(0, b"short").unwrap();
        res.commit(Some(5)).unwrap();

        let outcome = res.wait_range(5..10).unwrap();
        assert_eq!(outcome, WaitOutcome::Eof);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_fail_wakes_waiters() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        let res2 = res.clone();

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            res2.fail("test error".to_string());
        });

        let result = res.wait_range(0..100);
        assert!(result.is_err());
        handle.join().unwrap();
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
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

        let handle = thread::spawn({
            let cancel = cancel.clone();
            move || {
                thread::sleep(Duration::from_millis(50));
                cancel.cancel();
            }
        });

        let result = res.wait_range(0..100);
        assert!(matches!(result, Err(StorageError::Cancelled)));
        handle.join().unwrap();
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_status_failed() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }
}
