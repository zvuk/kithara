#![forbid(unsafe_code)]

//! In-memory storage driver backed by a growable byte pool buffer.
//!
//! [`MemDriver`] implements [`Driver`](crate::Driver) using a pool-managed
//! `Vec<u8>` from [`byte_pool()`](kithara_bufpool::byte_pool).
//! All written data is retained (no eviction) — [`DriverIo::valid_window()`]
//! returns `None`, meaning backward seeks never lose data.

use std::{fmt, path::Path};

use kithara_bufpool::{PooledOwned, byte_pool};
use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    StorageError, StorageResult,
    driver::{Driver, DriverIo, DriverState, Resource},
};

/// Options for creating a [`MemResource`].
#[derive(Debug, Clone, Default)]
pub struct MemOptions {
    /// Pre-fill the resource with this data (committed on creation).
    pub initial_data: Option<Vec<u8>>,
    /// Initial capacity hint in bytes.
    /// The buffer starts with this capacity but grows as needed on writes.
    /// Defaults to 0 (start empty, grow on demand).
    pub capacity: usize,
}

/// Internal state of the growable memory driver.
struct MemState {
    /// Pool-managed byte buffer. Grows via `ensure_len()`.
    buf: PooledOwned<32, Vec<u8>>,
    /// Logical length: highest write extent across all writes.
    len: u64,
}

/// In-memory storage driver backed by a growable byte pool buffer.
///
/// Uses [`byte_pool()`](kithara_bufpool::byte_pool) for memory management
/// with byte budget enforcement. Data is never evicted —
/// [`valid_window()`](DriverIo::valid_window) returns `None`.
///
/// `path()` returns `None`.
pub struct MemDriver {
    state: Mutex<MemState>,
}

impl fmt::Debug for MemDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock_sync();
        f.debug_struct("MemDriver")
            .field("len", &state.len)
            .field("capacity", &state.buf.capacity())
            .finish()
    }
}

impl Driver for MemDriver {
    type Options = MemOptions;

    fn open(opts: MemOptions) -> StorageResult<(Self, DriverState)> {
        let mut buf = byte_pool().get();

        let (len, init_state) = if let Some(data) = opts.initial_data {
            let data_len = data.len();
            if data_len > 0 {
                buf.ensure_len(data_len)
                    .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
                buf[..data_len].copy_from_slice(&data);
            }
            let len = data_len as u64;
            let mut available = rangemap::RangeSet::new();
            if len > 0 {
                available.insert(0..len);
            }
            (
                len,
                DriverState {
                    available,
                    committed: true,
                    final_len: Some(len),
                },
            )
        } else {
            if opts.capacity > 0 {
                buf.ensure_len(opts.capacity)
                    .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
            }
            (0, DriverState::default())
        };

        let driver = Self {
            state: Mutex::new(MemState { buf, len }),
        };

        Ok((driver, init_state))
    }
}

impl DriverIo for MemDriver {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        let state = self.state.lock_sync();

        if offset >= state.len {
            return Ok(0);
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to len which fits in memory"
        )]
        let available = (state.len - offset) as usize;
        let to_read = buf.len().min(available);

        #[expect(
            clippy::cast_possible_truncation,
            reason = "offset < len which fits in memory"
        )]
        let start = offset as usize;
        buf[..to_read].copy_from_slice(&state.buf[start..start + to_read]);
        drop(state);

        Ok(to_read)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        if committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        let mut state = self.state.lock_sync();
        let end = offset + data.len() as u64;

        #[expect(
            clippy::cast_possible_truncation,
            reason = "bounded by byte budget (256 MB)"
        )]
        let end_usize = end as usize;

        // Grow buffer if write extends beyond current allocation.
        if end_usize > state.buf.len() {
            state
                .buf
                .ensure_len(end_usize)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "offset < end which fits in memory"
        )]
        let start = offset as usize;
        state.buf[start..end_usize].copy_from_slice(data);

        if end > state.len {
            state.len = end;
        }
        drop(state);

        Ok(())
    }

    fn commit(&self, _final_len: Option<u64>) -> StorageResult<()> {
        Ok(())
    }

    fn reactivate(&self) -> StorageResult<()> {
        Ok(())
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn storage_len(&self) -> u64 {
        let state = self.state.lock_sync();
        state.len
    }

    // valid_window() returns None (default) — no eviction, all data retained.
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
                ..MemOptions::default()
            },
        )
        .expect("MemDriver::open with initial_data should never fail")
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    #[cfg(not(target_arch = "wasm32"))]
    use kithara_platform::thread;
    use kithara_platform::time::Duration;

    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::StorageError;
    use crate::resource::{ResourceExt, ResourceStatus, WaitOutcome};

    fn create_resource() -> MemResource {
        MemResource::new(CancellationToken::new())
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_create_new_resource() {
        let res = create_resource();
        assert_eq!(res.len(), None);
        assert_eq!(res.status(), ResourceStatus::Active);
        assert_eq!(res.path(), None);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_write_and_read() {
        let res = create_resource();

        res.write_at(0, b"hello world").unwrap();
        res.commit(Some(11)).unwrap();

        let mut buf = [0u8; 11];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_write_all_read_into() {
        let res = create_resource();

        res.write_all(b"atomic data").unwrap();

        let mut buf = Vec::new();
        let n = res.read_into(&mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf[..], b"atomic data");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_wait_range_ready() {
        let res = create_resource();

        res.write_at(0, b"data").unwrap();

        let outcome = res.wait_range(0..4).unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
    }

    #[kithara::test(native)]
    fn test_wait_range_blocks_then_ready() {
        let res = create_resource();
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
        let res = create_resource();

        res.write_at(0, b"short").unwrap();
        res.commit(Some(5)).unwrap();

        let outcome = res.wait_range(5..10).unwrap();
        assert_eq!(outcome, WaitOutcome::Eof);
    }

    #[kithara::test(native)]
    fn test_fail_wakes_waiters() {
        let res = create_resource();
        let res2 = res.clone();

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            res2.fail("test error".to_string());
        });

        let result = res.wait_range(0..100);
        assert!(result.is_err());
        handle.join().unwrap();
    }

    #[kithara::test(native)]
    fn test_cancel_wakes_waiters() {
        let cancel = CancellationToken::new();
        let res = MemResource::new(cancel.clone());

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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_status_failed() {
        let res = create_resource();

        res.fail("boom".to_string());
        assert_eq!(res.status(), ResourceStatus::Failed("boom".to_string()));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_write_rejected_after_commit() {
        let res = create_resource();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();

        let result = res.write_at(0, b"nope");
        assert!(result.is_err());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
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

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_growable_write_beyond_initial_capacity() {
        // Start with a small capacity hint, then write beyond it.
        let res = MemResource::open(
            CancellationToken::new(),
            MemOptions {
                capacity: 64,
                ..Default::default()
            },
        )
        .unwrap();

        // Write 128 bytes — beyond the 64-byte initial capacity.
        let data = vec![0xAB; 128];
        res.write_at(0, &data).unwrap();

        let mut buf = vec![0u8; 128];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 128);
        assert!(buf.iter().all(|b| *b == 0xAB));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_growable_sparse_write() {
        let res = create_resource();

        // Write at a large offset — buffer auto-extends.
        res.write_at(1000, b"far away").unwrap();

        let mut buf = [0u8; 8];
        let n = res.read_at(1000, &mut buf).unwrap();
        assert_eq!(n, 8);
        assert_eq!(&buf, b"far away");

        // Earlier offsets are zero-filled.
        let mut zero_buf = [0xFFu8; 4];
        let n = res.read_at(0, &mut zero_buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&zero_buf, &[0, 0, 0, 0]);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_growable_multiple_writes_extend() {
        let res = create_resource();

        // Sequential writes that extend the buffer.
        res.write_at(0, b"aaa").unwrap();
        res.write_at(3, b"bbb").unwrap();
        res.write_at(6, b"ccc").unwrap();

        let mut buf = [0u8; 9];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"aaabbbccc");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_from_bytes_readable() {
        // from_bytes creates a committed resource — the data should be readable.
        let data = b"hello growable buffer world";
        let res = MemResource::from_bytes(data, CancellationToken::new());

        let mut buf = vec![0u8; data.len()];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&buf, data);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_backward_write_does_not_lose_data() {
        let res = create_resource();

        // Write forward.
        res.write_at(0, &[0xAA; 100]).unwrap();
        // Write at a later offset.
        res.write_at(200, &[0xBB; 100]).unwrap();
        // Write backward — should NOT evict earlier data.
        res.write_at(50, &[0xCC; 50]).unwrap();

        // All three regions should be readable.
        let mut buf = [0u8; 10];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, &[0xAA; 10]);

        let n = res.read_at(50, &mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, &[0xCC; 10]);

        let n = res.read_at(200, &mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, &[0xBB; 10]);
    }
}
