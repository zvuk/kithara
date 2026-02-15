#![forbid(unsafe_code)]

//! In-memory storage driver backed by a ring buffer.
//!
//! [`MemDriver`] implements [`Driver`](crate::Driver) using a power-of-2 ring
//! buffer with bitmask indexing for O(1) wrap-around. Intended for WASM
//! (wasm32 + atomics) where mmap is unavailable.
//!
//! When writes wrap around and evict old data, the `available` `RangeSet` in
//! `CommonState` is updated (via [`Driver::valid_window`]) to remove evicted
//! ranges. This allows `wait_range()` to detect gaps and trigger re-fetches
//! on seek.

use std::{ops::Range, path::Path};

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    StorageError, StorageResult,
    driver::{Driver, DriverState, Resource},
};

/// Default ring buffer capacity (1 mebibyte, power of 2).
const DEFAULT_RING_CAPACITY: usize = 1 << 20;

/// Options for creating a [`MemResource`].
#[derive(Debug, Clone)]
pub struct MemOptions {
    /// Pre-fill the resource with this data (committed on creation).
    pub initial_data: Option<Vec<u8>>,
    /// Ring buffer capacity in bytes. Must be power of 2.
    /// Defaults to 1 mebibyte. Rounded up to next power of 2 if not already.
    pub capacity: usize,
}

impl Default for MemOptions {
    fn default() -> Self {
        Self {
            initial_data: None,
            capacity: DEFAULT_RING_CAPACITY,
        }
    }
}

/// Internal state of the ring buffer driver.
struct RingState {
    /// Buffer data (power-of-2 sized).
    buf: Vec<u8>,
    /// Start of valid data window (absolute byte offset).
    /// Data before this offset has been evicted by ring wrap-around.
    window_start: u64,
    /// Capacity (always power of 2).
    capacity: usize,
}

/// In-memory storage driver backed by a ring buffer.
///
/// Uses power-of-2 bitmask indexing for O(1) wrap-around.
/// `path()` returns `None`.
pub struct MemDriver {
    state: Mutex<RingState>,
}

impl std::fmt::Debug for MemDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        f.debug_struct("MemDriver")
            .field("capacity", &state.capacity)
            .field("window_start", &state.window_start)
            .finish()
    }
}

impl Driver for MemDriver {
    type Options = MemOptions;

    fn open(opts: MemOptions) -> StorageResult<(Self, DriverState)> {
        let (ring, init) = if let Some(data) = opts.initial_data {
            let len = data.len() as u64;
            let mut available = rangemap::RangeSet::new();
            if len > 0 {
                available.insert(0..len);
            }
            // For pre-filled data: capacity must be at least data.len().
            let actual_capacity = opts.capacity.max(data.len()).next_power_of_two();
            let mut buf = vec![0u8; actual_capacity];
            buf[..data.len()].copy_from_slice(&data);
            (
                RingState {
                    buf,
                    window_start: 0,
                    capacity: actual_capacity,
                },
                DriverState {
                    available,
                    committed: true,
                    final_len: Some(len),
                },
            )
        } else {
            let capacity = opts.capacity.next_power_of_two();
            let buf = vec![0u8; capacity];
            (
                RingState {
                    buf,
                    window_start: 0,
                    capacity,
                },
                DriverState::default(),
            )
        };

        let driver = Self {
            state: Mutex::new(ring),
        };

        Ok((driver, init))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        let ring = self.state.lock();
        let window_end = ring.window_start + ring.capacity as u64;

        // Check offset is within the valid window.
        if offset < ring.window_start || offset >= window_end {
            return Ok(0);
        }

        // Clamp read length to not exceed window end.
        #[expect(clippy::cast_possible_truncation)] // within ring capacity
        let available = (window_end - offset) as usize;
        let to_read = buf.len().min(available);

        let mask = ring.capacity - 1;
        #[expect(clippy::cast_possible_truncation)] // bitmask index within capacity
        let start_idx = (offset as usize) & mask;

        // Check if read wraps around the ring buffer.
        let first_part = ring.capacity - start_idx;
        if to_read <= first_part {
            // No wrap: single copy.
            buf[..to_read].copy_from_slice(&ring.buf[start_idx..start_idx + to_read]);
        } else {
            // Wrap: two copies.
            buf[..first_part].copy_from_slice(&ring.buf[start_idx..]);
            let second_part = to_read - first_part;
            buf[first_part..to_read].copy_from_slice(&ring.buf[..second_part]);
        }
        drop(ring);

        Ok(to_read)
    }

    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        if committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        let mut ring = self.state.lock();
        let mask = ring.capacity - 1;

        #[expect(clippy::cast_possible_truncation)] // bitmask index within capacity
        let start_idx = (offset as usize) & mask;

        // Copy with wrap-around.
        let first_part = ring.capacity - start_idx;
        if data.len() <= first_part {
            // No wrap: single copy.
            ring.buf[start_idx..start_idx + data.len()].copy_from_slice(data);
        } else {
            // Wrap: two copies.
            ring.buf[start_idx..].copy_from_slice(&data[..first_part]);
            let second_part = data.len() - first_part;
            ring.buf[..second_part].copy_from_slice(&data[first_part..]);
        }

        // Advance window_start if write extends past current window end.
        let write_end = offset + data.len() as u64;
        let window_end = ring.window_start + ring.capacity as u64;
        if write_end > window_end {
            ring.window_start = write_end - ring.capacity as u64;
        }
        drop(ring);

        Ok(())
    }

    fn commit(&self, _final_len: Option<u64>) -> StorageResult<()> {
        // Ring buffer does not truncate.
        Ok(())
    }

    fn reactivate(&self) -> StorageResult<()> {
        // Buffer data preserved.
        Ok(())
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn storage_len(&self) -> u64 {
        let ring = self.state.lock();
        ring.window_start + ring.capacity as u64
    }

    fn valid_window(&self) -> Option<Range<u64>> {
        let ring = self.state.lock();
        Some(ring.window_start..ring.window_start + ring.capacity as u64)
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
                ..MemOptions::default()
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
