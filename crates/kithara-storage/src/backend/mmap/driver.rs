#![forbid(unsafe_code)]

use std::{fmt, fs, ops::Range, path::PathBuf};

use arc_swap::ArcSwapOption;
use bon::Builder;
use crossbeam_queue::SegQueue;
use kithara_platform::sync::{Arc, Mutex};
use mmap_io::MemoryMappedFile;
use rangemap::RangeSet;

use crate::{
    StorageResult,
    backend::{
        resource::ResourceWriter,
        traits::{Driver, DriverState},
    },
    resource::OpenMode,
};

/// Options for opening a [`MmapResource`].
#[derive(Debug, Clone, Builder)]
#[builder(start_fn = for_path)]
#[non_exhaustive]
pub struct MmapOptions {
    /// Path to the backing file.
    #[builder(start_fn)]
    pub path: PathBuf,
    /// Open mode controlling read/write behavior for existing files.
    #[builder(default)]
    pub mode: OpenMode,
    /// Multiplier applied to the current mapping length when a write runs
    /// past its end. The mapping grows to the larger of the write's end and
    /// `len * growth_factor`, so a factor of 1 grows to exactly what each
    /// write needs and re-maps on every one. The default doubles, which keeps
    /// the number of re-maps logarithmic in the final size.
    #[builder(default = 2)]
    pub growth_factor: u64,
    /// Size a new file is created at. Ignored for existing files. The default
    /// is one page-aligned block: enough that a small resource is written
    /// without a single re-map, small enough that a resource that turns out to
    /// be empty costs one sparse block.
    #[builder(default = 64 * 1024)]
    pub initial_len: u64,
}

/// Mmap state machine.
///
/// - `Active`: read-write mmap, used during streaming/writing.
/// - `Committed`: read-only mmap, after commit (no writes allowed).
/// - `Empty`: zero-length committed resource (no mmap needed).
pub(super) enum MmapState {
    Active(MemoryMappedFile),
    Committed(Arc<MemoryMappedFile>),
    Empty,
}

impl MmapState {
    pub(super) fn as_readable(&self) -> Option<&MemoryMappedFile> {
        match self {
            Self::Active(m) => Some(m),
            Self::Committed(m) => Some(m.as_ref()),
            Self::Empty => None,
        }
    }
    pub(super) fn len(&self) -> u64 {
        match self {
            Self::Active(m) => m.len(),
            Self::Committed(m) => m.len(),
            Self::Empty => 0,
        }
    }
}

/// Mmap-backed storage driver.
///
/// Uses `mmap-io` for file-backed storage with a lock-free `SegQueue`
/// for fast-path wait notifications.
pub struct MmapDriver {
    /// Immutable committed snapshot for the lock-free read fast path.
    pub(super) committed: ArcSwapOption<MemoryMappedFile>,
    pub(super) mmap: Mutex<MmapState>,
    pub(super) mode: OpenMode,
    pub(super) path: PathBuf,
    /// Lock-free queue for fast-path range notifications.
    pub(super) ready_ranges: SegQueue<Range<u64>>,
    /// Multiplier a write past the mapping's end grows it by, from
    /// `MmapOptions::growth_factor`.
    pub(super) growth_factor: u64,
    /// Size a fresh mapping starts at, from `MmapOptions::initial_len`. A
    /// re-download reuses it so the rewrite generation is reserved exactly
    /// like the first one instead of restarting from the default and
    /// re-mapping its way back up.
    pub(super) initial_len: u64,
}

impl fmt::Debug for MmapDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MmapDriver")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .field("committed", &self.committed.load().is_some())
            .finish_non_exhaustive()
    }
}

impl Driver for MmapDriver {
    type Options = MmapOptions;

    fn open(opts: MmapOptions) -> StorageResult<(Self, DriverState)> {
        let mode = opts.mode;

        let (mmap_state, init, committed) =
            if opts.path.exists() && fs::metadata(&opts.path)?.len() > 0 {
                let len;
                let (mmap_state, snapshot) = if mode == OpenMode::ReadWrite {
                    let mmap = MemoryMappedFile::open_rw(&opts.path)?;
                    len = mmap.len();
                    (MmapState::Active(mmap), None)
                } else {
                    let arc = Arc::new(MemoryMappedFile::open_ro(&opts.path)?);
                    len = arc.len();
                    (MmapState::Committed(Arc::clone(&arc)), Some(arc))
                };
                let mut available = RangeSet::new();
                available.insert(0..len);
                let init = DriverState {
                    available,
                    is_committed: true,
                    final_len: Some(len),
                };
                (mmap_state, init, ArcSwapOption::from(snapshot))
            } else if mode == OpenMode::ReadOnly {
                (
                    MmapState::Empty,
                    DriverState {
                        available: RangeSet::new(),
                        is_committed: true,
                        final_len: Some(0),
                    },
                    ArcSwapOption::empty(),
                )
            } else {
                if let Some(parent) = opts.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let size = opts.initial_len;
                let mmap_state = if size == 0 {
                    MmapState::Empty
                } else {
                    let mmap = MemoryMappedFile::create_rw(&opts.path, size)?;
                    MmapState::Active(mmap)
                };
                (mmap_state, DriverState::default(), ArcSwapOption::empty())
            };

        let driver = Self {
            mode,
            committed,
            mmap: Mutex::new(mmap_state),
            path: opts.path,
            initial_len: opts.initial_len,
            growth_factor: opts.growth_factor,
            ready_ranges: SegQueue::new(),
        };

        Ok((driver, init))
    }
}

/// Mmap-backed storage resource.
///
/// Type alias for [`ResourceWriter<MmapDriver>`].
pub type MmapResource = ResourceWriter<MmapDriver>;

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use kithara_platform::{CancelToken, thread, time::Duration};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        Resource, ResourceRead, StorageError,
        backend::traits::DriverIo,
        resource::{ResourceStatus, WaitOutcome},
    };

    fn create_resource(dir: &TempDir) -> MmapResource {
        create_resource_with_size(dir, None)
    }

    fn create_resource_with_size(dir: &TempDir, size: Option<u64>) -> MmapResource {
        let path = dir.path().join("test.dat");
        Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(path)
                .mode(OpenMode::Auto)
                .maybe_initial_len(size)
                .build(),
        )
        .expect("BUG: open test resource with hard-coded params must succeed")
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
        let res = res.commit(Some(11)).unwrap();

        let mut buf = [0u8; 11];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_write_all_read_into() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        let res = res.write_all(b"atomic data").unwrap();

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
        let reader = res.reader();

        // Return the writer from the thread so it outlives the read: dropping an
        // uncommitted writer now marks the resource failed (anti-hang), which
        // would otherwise race the availability notify.
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            res.write_at(0, b"delayed data").unwrap();
            res
        });

        let outcome = reader.wait_range(0..12).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
        let _writer = handle.join().unwrap();
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_wait_range_eof() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        res.write_at(0, b"short").unwrap();
        let res = res.commit(Some(5)).unwrap();

        let outcome = res.wait_range(5..10).unwrap();
        assert_eq!(outcome, WaitOutcome::Eof);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_fail_wakes_waiters() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        let reader = res.reader();

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            res.fail("test error".to_string());
        });

        let result = reader.wait_range(0..100);
        assert!(result.is_err());
        handle.join().unwrap();
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn test_cancel_wakes_waiters() {
        let dir = TempDir::new().unwrap();
        let cancel = CancelToken::never();
        let path = dir.path().join("cancel_test.dat");

        let res: MmapResource = Resource::open(
            cancel.clone(),
            MmapOptions::for_path(path).mode(OpenMode::Auto).build(),
        )
        .expect("BUG: open cancel-test resource with hard-coded params must succeed");

        let handle = thread::spawn({
            let cancel = cancel;
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
                CancelToken::never(),
                MmapOptions::for_path(path.clone())
                    .mode(OpenMode::Auto)
                    .build(),
            )
            .expect("BUG: opening the first resource in this test setup must succeed");
            res.write_all(b"persisted data").unwrap();
        }

        let res: MmapResource = Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(path).mode(OpenMode::Auto).build(),
        )
        .expect("BUG: re-opening the resource in this test setup must succeed");

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
        let res = res.commit(Some(1024)).unwrap();

        let mut buf = vec![0u8; 1024];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 1024);
        assert!(buf.iter().all(|&b| b == 42));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn failed_post_commit_reopen_keeps_snapshot() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("original.dat");
        let (mut driver, _) = MmapDriver::open(
            MmapOptions::for_path(path)
                .mode(OpenMode::ReadWrite)
                .build(),
        )
        .unwrap();
        driver.write_at(0, b"data", false).unwrap();
        driver.commit(Some(4)).unwrap();

        driver.path = dir.path().join("missing").join("resource.dat");
        assert!(driver.write_at(0, b"lost", true).is_err());

        let mut buf = [0; 4];
        assert_eq!(driver.read_committed(0, &mut buf).unwrap(), Some(4));
        assert_eq!(&buf, b"data");
    }

    fn create_resource_growing_by(dir: &TempDir, initial: u64, factor: u64) -> MmapResource {
        Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(dir.path().join("test.dat"))
                .initial_len(initial)
                .growth_factor(factor)
                .build(),
        )
        .expect("BUG: open test resource with hard-coded params must succeed")
    }

    /// The growth factor is the caller's: at 1 a write past the mapping's end
    /// grows it to exactly that end and nothing more.
    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn a_growth_factor_of_one_grows_to_exactly_the_write() {
        let dir = TempDir::new().unwrap();
        let res = create_resource_growing_by(&dir, 64, 1);

        res.write_at(0, &[7u8; 100]).unwrap();

        assert_eq!(
            fs::metadata(dir.path().join("test.dat")).unwrap().len(),
            100
        );
    }

    /// Left unset, the default overshoots instead, so the same write leaves
    /// room for the next one rather than re-mapping on every write.
    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn the_default_growth_factor_overshoots_the_write() {
        let dir = TempDir::new().unwrap();
        let res: MmapResource = Resource::open(
            CancelToken::never(),
            MmapOptions::for_path(dir.path().join("test.dat"))
                .initial_len(64)
                .build(),
        )
        .expect("BUG: open test resource with hard-coded params must succeed");

        res.write_at(0, &[7u8; 100]).unwrap();

        assert_eq!(
            fs::metadata(dir.path().join("test.dat")).unwrap().len(),
            128
        );
    }

    /// Sealing finalizes the bytes without publishing a snapshot, because the
    /// caller is about to rename the file and republish it itself. Until that
    /// happens the live mapping is all a reader has, so it must keep serving.
    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn a_sealed_resource_still_serves_its_bytes() {
        let dir = TempDir::new().unwrap();
        let res = create_resource_with_size(&dir, Some(64 * 1024));
        res.write_at(0, b"payload").unwrap();

        res.seal_in_place(Some(7)).unwrap();

        let mut buf = [0u8; 7];
        res.reader().read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"payload", "a sealed resource must stay readable");
    }

    /// A re-download starts a fresh generation in a rewrite temp file. It
    /// carries the same payload as the first one, so it must start from the
    /// same reservation — otherwise the rewrite re-maps its way back up from
    /// the default while the original never had to.
    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn rewrite_generation_reuses_the_original_reservation() {
        let dir = TempDir::new().unwrap();
        let reservation = 1024 * 1024;
        let res = create_resource_with_size(&dir, Some(reservation));
        res.write_at(0, b"first").unwrap();
        let res = res.commit(Some(5)).unwrap();

        let _writer = res.reactivate().unwrap();

        let rewrite = dir.path().join("test.dat.kithara-rewrite");
        assert_eq!(
            fs::metadata(&rewrite).unwrap().len(),
            reservation,
            "the rewrite generation must be reserved like the first one"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_status_transitions() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);

        assert_eq!(res.status(), ResourceStatus::Active);

        res.write_at(0, b"data").unwrap();
        assert_eq!(res.status(), ResourceStatus::Active);

        let res = res.commit(Some(4)).unwrap();
        assert_eq!(
            res.status(),
            ResourceStatus::Committed { final_len: Some(4) }
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_status_failed() {
        let dir = TempDir::new().unwrap();
        let res = create_resource(&dir);
        let reader = res.reader();

        res.fail("boom".to_string());
        assert_eq!(reader.status(), ResourceStatus::Failed("boom".to_string()));
    }
}
