#![forbid(unsafe_code)]

//! In-memory storage resource for platforms without filesystem access.
//!
//! `MemoryResource` implements [`ResourceExt`] backed by a `Vec<u8>` instead of
//! mmap. Uses the same `RangeSet<u64>` + `Condvar` synchronization as
//! [`StorageResource`](crate::StorageResource), so `wait_range` works identically.
//!
//! Intended for WASM (wasm32 + atomics) where mmap is unavailable.

use std::{ops::Range, path::Path, sync::Arc};

use parking_lot::{Condvar, Mutex};
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;

use crate::{ResourceExt, ResourceStatus, StorageError, StorageResult, WaitOutcome};

/// Internal state protected by Mutex.
struct MemState {
    buf: Vec<u8>,
    available: RangeSet<u64>,
    committed: bool,
    final_len: Option<u64>,
    failed: Option<String>,
}

impl Default for MemState {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            available: RangeSet::new(),
            committed: false,
            final_len: None,
            failed: None,
        }
    }
}

struct MemInner {
    state: Mutex<MemState>,
    condvar: Condvar,
    cancel: CancellationToken,
}

/// In-memory storage resource.
///
/// Drop-in replacement for [`StorageResource`](crate::StorageResource) on
/// platforms without filesystem access. All data lives in a `Vec<u8>` protected
/// by a `Mutex`; `wait_range` blocks via `Condvar`, same as the mmap variant.
///
/// `path()` returns `None`.
#[derive(Clone)]
pub struct MemoryResource {
    inner: Arc<MemInner>,
}

impl std::fmt::Debug for MemoryResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock();
        f.debug_struct("MemoryResource")
            .field("len", &state.buf.len())
            .field("committed", &state.committed)
            .finish()
    }
}

impl MemoryResource {
    /// Create a new empty in-memory resource.
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            inner: Arc::new(MemInner {
                state: Mutex::new(MemState::default()),
                condvar: Condvar::new(),
                cancel,
            }),
        }
    }

    /// Create a committed resource pre-filled with data.
    pub fn from_bytes(data: &[u8], cancel: CancellationToken) -> Self {
        let len = data.len() as u64;
        let mut available = RangeSet::new();
        if len > 0 {
            available.insert(0..len);
        }
        Self {
            inner: Arc::new(MemInner {
                state: Mutex::new(MemState {
                    buf: data.to_vec(),
                    available,
                    committed: true,
                    final_len: Some(len),
                    failed: None,
                }),
                condvar: Condvar::new(),
                cancel,
            }),
        }
    }

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

impl ResourceExt for MemoryResource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.check_health()?;

        let state = self.inner.state.lock();
        let data_len = state.final_len.unwrap_or(state.buf.len() as u64);
        let effective_len = data_len.min(state.buf.len() as u64);

        if offset >= effective_len {
            return Ok(0);
        }

        let available = (effective_len - offset) as usize;
        let to_read = buf.len().min(available);
        let start = offset as usize;
        buf[..to_read].copy_from_slice(&state.buf[start..start + to_read]);

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

        let mut state = self.inner.state.lock();

        if state.committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        // Grow buffer if needed.
        let end_usize = end as usize;
        if state.buf.len() < end_usize {
            state.buf.resize(end_usize, 0);
        }

        let start = offset as usize;
        state.buf[start..end_usize].copy_from_slice(data);
        state.available.insert(offset..end);

        self.inner.condvar.notify_all();
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

        let mut state = self.inner.state.lock();

        loop {
            if self.inner.cancel.is_cancelled() {
                return Err(StorageError::Cancelled);
            }

            if let Some(ref reason) = state.failed {
                return Err(StorageError::Failed(reason.clone()));
            }

            if range_covered_by(&state.available, &range) {
                return Ok(WaitOutcome::Ready);
            }

            if state.committed {
                let final_len = state.final_len.unwrap_or(0);
                if range.start >= final_len {
                    return Ok(WaitOutcome::Eof);
                }
                return Ok(WaitOutcome::Ready);
            }

            self.inner
                .condvar
                .wait_for(&mut state, std::time::Duration::from_millis(50));
        }
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;

        let mut state = self.inner.state.lock();

        // Truncate buffer if final_len is smaller.
        if let Some(len) = final_len {
            let len_usize = len as usize;
            if state.buf.len() > len_usize {
                state.buf.truncate(len_usize);
            }
            if len > 0 {
                state.available.insert(0..len);
            }
        }

        state.committed = true;
        state.final_len = final_len;

        self.inner.condvar.notify_all();
        Ok(())
    }

    fn fail(&self, reason: String) {
        let mut state = self.inner.state.lock();
        state.failed = Some(reason);
        self.inner.condvar.notify_all();
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn len(&self) -> Option<u64> {
        let state = self.inner.state.lock();
        state.final_len
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.check_health()?;

        let mut state = self.inner.state.lock();
        state.committed = false;
        state.final_len = None;
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

    use super::*;

    fn create_resource() -> MemoryResource {
        MemoryResource::new(CancellationToken::new())
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
        let res = MemoryResource::from_bytes(b"preloaded", CancellationToken::new());

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
        let res = MemoryResource::new(cancel.clone());

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
