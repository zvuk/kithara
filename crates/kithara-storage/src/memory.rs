#![forbid(unsafe_code)]

//! In-memory storage driver for platforms without filesystem access.
//!
//! [`MemDriver`] implements [`Driver`](crate::Driver) backed by a `Vec<u8>`
//! instead of mmap. Intended for WASM (wasm32 + atomics) where mmap is unavailable.

use std::path::Path;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    StorageError, StorageResult,
    driver::{Driver, DriverState, Resource},
};

/// Options for creating a [`MemResource`].
#[derive(Debug, Clone, Default)]
pub struct MemOptions {
    /// Pre-fill the resource with this data (committed on creation).
    pub initial_data: Option<Vec<u8>>,
}

/// In-memory storage driver.
///
/// All data lives in a `Vec<u8>` protected by a `Mutex`.
/// `path()` returns `None`.
pub struct MemDriver {
    buf: Mutex<Vec<u8>>,
}

impl std::fmt::Debug for MemDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let buf = self.buf.lock();
        f.debug_struct("MemDriver")
            .field("len", &buf.len())
            .finish()
    }
}

impl Driver for MemDriver {
    type Options = MemOptions;

    fn open(opts: MemOptions) -> StorageResult<(Self, DriverState)> {
        let (buf, init) = opts.initial_data.map_or_else(
            || (Vec::new(), DriverState::default()),
            |data| {
                let len = data.len() as u64;
                let mut available = rangemap::RangeSet::new();
                if len > 0 {
                    available.insert(0..len);
                }
                (
                    data,
                    DriverState {
                        available,
                        committed: true,
                        final_len: Some(len),
                    },
                )
            },
        );

        let driver = Self {
            buf: Mutex::new(buf),
        };

        Ok((driver, init))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        let data = self.buf.lock();
        #[expect(clippy::cast_possible_truncation)] // byte offset within allocated buffer
        let start = offset as usize;
        if start >= data.len() {
            return Ok(0);
        }
        let available = data.len() - start;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&data[start..start + to_read]);
        drop(data);
        Ok(to_read)
    }

    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        if committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        #[expect(clippy::cast_possible_truncation)] // byte offset within allocated buffer
        let start = offset as usize;
        let end = start + data.len();
        let mut buf = self.buf.lock();
        if buf.len() < end {
            buf.resize(end, 0);
        }
        buf[start..end].copy_from_slice(data);
        drop(buf);
        Ok(())
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        if let Some(len) = final_len {
            #[expect(clippy::cast_possible_truncation)] // buffer size fits in memory
            let len_usize = len as usize;
            let mut buf = self.buf.lock();
            if buf.len() > len_usize {
                buf.truncate(len_usize);
            }
        }
        Ok(())
    }

    fn reactivate(&self) -> StorageResult<()> {
        // Buffer data preserved — nothing to do for in-memory driver.
        Ok(())
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn storage_len(&self) -> u64 {
        let buf = self.buf.lock();
        buf.len() as u64
    }
}

/// In-memory storage resource.
///
/// Type alias for [`Resource<MemDriver>`]. Drop-in replacement for
/// [`MmapResource`](crate::MmapResource) on platforms without filesystem access.
pub type MemResource = Resource<MemDriver>;

impl MemResource {
    /// Create a new empty in-memory resource.
    ///
    /// # Panics
    ///
    /// Panics if `MemDriver::open` fails (should never happen with default options).
    #[must_use]
    pub fn new(cancel: CancellationToken) -> Self {
        // MemDriver::open with default opts never fails.
        Self::open(cancel, MemOptions::default())
            .expect("MemDriver::open with default options should never fail")
    }

    /// Create a committed resource pre-filled with data.
    ///
    /// # Panics
    ///
    /// Panics if `MemDriver::open` fails (should never happen with initial data).
    #[must_use]
    pub fn from_bytes(data: &[u8], cancel: CancellationToken) -> Self {
        Self::open(
            cancel,
            MemOptions {
                initial_data: Some(data.to_vec()),
            },
        )
        .expect("MemDriver::open with initial_data should never fail")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::*;

    use super::*;
    use crate::{
        StorageError,
        resource::{ResourceExt, ResourceStatus, WaitOutcome},
    };

    fn create_resource() -> MemResource {
        MemResource::new(CancellationToken::new())
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_create_new_resource() {
        let res = create_resource();
        assert_eq!(res.len(), None);
        assert_eq!(res.status(), ResourceStatus::Active);
        assert_eq!(res.path(), None);
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_write_and_read() {
        let res = create_resource();

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
        let res = create_resource();

        res.write_all(b"atomic data").unwrap();

        let mut buf = Vec::new();
        let n = res.read_into(&mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf[..], b"atomic data");
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_from_bytes() {
        let res = MemResource::from_bytes(b"preloaded", CancellationToken::new());

        assert_eq!(
            res.status(),
            ResourceStatus::Committed { final_len: Some(9) }
        );
        assert_eq!(res.len(), Some(9));

        let mut buf = Vec::new();
        let n = res.read_into(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf[..], b"preloaded");
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_wait_range_ready() {
        let res = create_resource();

        res.write_at(0, b"data").unwrap();

        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn test_wait_range_blocks_then_ready() {
        let res = create_resource();
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
        let res = create_resource();

        res.write_at(0, b"short").unwrap();
        res.commit(Some(5)).unwrap();

        let outcome = res.wait_range(5..10).unwrap();
        assert_eq!(outcome, WaitOutcome::Eof);
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_fail_wakes_waiters() {
        let res = create_resource();
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
        let cancel = CancellationToken::new();
        let res = MemResource::new(cancel.clone());

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
    fn test_status_transitions() {
        let res = create_resource();

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
        let res = create_resource();

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_reactivate() {
        let res = create_resource();

        res.write_at(0, b"hello").unwrap();
        res.commit(Some(5)).unwrap();
        assert!(matches!(res.status(), ResourceStatus::Committed { .. }));

        res.reactivate().unwrap();
        assert_eq!(res.status(), ResourceStatus::Active);
        assert_eq!(res.len(), None);

        // Old data still readable.
        let mut buf = [0u8; 5];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");

        // Can write new data.
        res.write_at(5, b" world").unwrap();
        res.commit(Some(11)).unwrap();

        let mut buf2 = vec![0u8; 11];
        let n = res.read_at(0, &mut buf2).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf2[..], b"hello world");
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_write_rejected_after_commit() {
        let res = create_resource();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        let result = res.write_at(0, b"nope");
        assert!(result.is_err());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_sparse_write() {
        let res = create_resource();

        // Write at offset 100
        res.write_at(100, b"sparse").unwrap();

        // Range 0..100 is not available
        let mut buf = [0u8; 6];
        let n = res.read_at(100, &mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"sparse");

        // Zeros before the data
        let mut zero_buf = [0xFFu8; 4];
        let n = res.read_at(0, &mut zero_buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&zero_buf, &[0, 0, 0, 0]);
    }
}
