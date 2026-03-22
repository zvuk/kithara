#![forbid(unsafe_code)]

//! Generic `Resource<D>`, `DriverIo` and `Driver` traits.
//!
//! `DriverIo` abstracts backend-specific storage I/O (mmap, in-memory, etc.).
//! `Driver` adds backend creation (`open`).
//! `Resource<D>` owns the common state machine (range tracking, committed/failed
//! flags, condvar coordination, cancellation) and delegates I/O to the driver.

use std::{
    fmt::{self, Debug},
    ops::Range,
    path::Path,
    sync::Arc,
};

use derivative::Derivative;
use kithara_platform::{
    Condvar, Mutex,
    thread::yield_now,
    time::{Duration as PlatformDuration, Instant},
};
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    StorageError, StorageResult,
    resource::{ResourceExt, ResourceStatus, WaitOutcome, range_covered_by},
};

/// Condvar wait spin timeout in milliseconds.
const WAIT_SPIN_TIMEOUT_MS: u64 = 50;

/// Backend-specific storage operations.
///
/// Drivers handle raw byte I/O and storage lifecycle transitions.
/// The common state machine (range tracking, committed/failed flags,
/// condvar coordination, cancellation) is managed by [`Resource<D>`].
///
/// Each driver manages its own interior mutability (e.g. `Mutex<MmapState>`
/// for mmap, `Mutex<Vec<u8>>` for memory).
#[cfg_attr(any(test, feature = "test-utils"), unimock::unimock(api = DriverIoMock))]
pub trait DriverIo: Send + Sync + 'static {
    /// Read bytes at offset into `buf`.
    ///
    /// `effective_len` is the min of `final_len` and physical storage length,
    /// pre-computed by [`Resource<D>`]. The driver reads up to `effective_len`.
    ///
    /// # Errors
    ///
    /// Returns error if the underlying storage read fails.
    fn read_at(&self, offset: u64, buf: &mut [u8], effective_len: u64) -> StorageResult<usize>;

    /// Write bytes at offset.
    ///
    /// `committed` comes from common state — the driver decides whether to
    /// reject (e.g. memory) or reopen as read-write (e.g. mmap `ReadWrite` mode).
    ///
    /// # Errors
    ///
    /// Returns error if the write fails or the resource is committed
    /// and the driver does not support post-commit writes.
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()>;

    /// Finalize backing store (e.g. mmap: resize + reopen read-only; memory: truncate).
    ///
    /// # Errors
    ///
    /// Returns error if the backing store cannot be finalized (e.g. file
    /// truncation or read-only reopen fails).
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;

    /// Reopen for writing (e.g. mmap: reopen as read-write; memory: no-op).
    ///
    /// # Errors
    ///
    /// Returns error if the backing store cannot be reopened for writing.
    fn reactivate(&self) -> StorageResult<()>;

    /// Filesystem path, if any.
    fn path(&self) -> Option<&Path>;

    /// Physical storage length (for `read_at` clamping).
    fn storage_len(&self) -> u64;

    /// Lock-free fast-path check before state mutex.
    ///
    /// Some drivers (mmap) maintain a lock-free queue of ready ranges.
    /// Called before acquiring the state mutex in `wait_range`.
    /// Return `true` if the range is definitely covered.
    fn try_fast_check(&self, _range: &Range<u64>) -> bool {
        false
    }

    /// Publish a write range to the fast-path mechanism.
    ///
    /// Called by [`Resource<D>::write_at`] after a successful driver write,
    /// before updating state + condvar.
    fn notify_write(&self, _range: &Range<u64>) {}

    /// Returns the valid data window for ring buffer drivers.
    ///
    /// When `Some(window)`, any ranges before `window.start` have been evicted
    /// and should be removed from the available set. Returns `None` for
    /// linear drivers where all written data is retained.
    fn valid_window(&self) -> Option<Range<u64>> {
        None
    }
}

/// Driver factory + I/O contract.
///
/// `Driver` inherits the runtime operations from [`DriverIo`] and adds creation.
pub trait Driver: DriverIo {
    /// Configuration needed to open/create a driver instance.
    type Options: Send;

    /// Open or create a new driver from options.
    ///
    /// Returns the driver and initial state to populate [`Resource<D>`].
    ///
    /// # Errors
    ///
    /// Returns error if the backing storage cannot be opened or created.
    fn open(opts: Self::Options) -> StorageResult<(Self, DriverState)>
    where
        Self: Sized;
}

/// Initial state returned by [`Driver::open`] to populate [`Resource<D>`].
#[derive(Default)]
pub struct DriverState {
    /// Pre-populated available byte ranges.
    pub available: RangeSet<u64>,
    /// Whether the resource starts as committed.
    pub committed: bool,
    /// Known final length (if committed).
    pub final_len: Option<u64>,
}

/// Common state tracked by `Resource<D>`.
struct CommonState {
    available: RangeSet<u64>,
    committed: bool,
    final_len: Option<u64>,
    failed: Option<String>,
}

/// Shared inner storage.
struct Inner<D: DriverIo> {
    driver: D,
    state: Mutex<CommonState>,
    condvar: Condvar,
    cancel: CancellationToken,
}

/// Generic storage resource parameterized by backend driver.
///
/// Owns the common state machine (range tracking, committed/failed flags,
/// condvar wait coordination, cancellation) and delegates backend-specific
/// I/O to `D`.
///
/// Use via type aliases:
/// - [`MmapResource`](crate::MmapResource) = `Resource<MmapDriver>`
/// - [`MemResource`](crate::MemResource) = `Resource<MemDriver>`
#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub struct Resource<D: DriverIo> {
    inner: Arc<Inner<D>>,
}

impl<D: DriverIo + Debug> Debug for Resource<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state.lock_sync();
        f.debug_struct("Resource")
            .field("driver", &self.inner.driver)
            .field("committed", &state.committed)
            .field("final_len", &state.final_len)
            .finish()
    }
}

impl<D: Driver> Resource<D> {
    /// Create a resource from driver options.
    ///
    /// # Errors
    ///
    /// Returns error if the driver cannot be opened (e.g. file I/O failure).
    pub fn open(cancel: CancellationToken, opts: D::Options) -> StorageResult<Self> {
        let (driver, init) = D::open(opts)?;
        Ok(Self {
            inner: Arc::new(Inner {
                driver,
                state: Mutex::new(CommonState {
                    available: init.available,
                    committed: init.committed,
                    final_len: init.final_len,
                    failed: None,
                }),
                condvar: Condvar::new(),
                cancel,
            }),
        })
    }
}

impl<D: DriverIo> Resource<D> {
    /// Check if the resource is cancelled or failed, returning error if so.
    fn check_health(&self) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }
        let failed = {
            let state = self.inner.state.lock_sync();
            state.failed.clone()
        };
        if let Some(reason) = failed {
            return Err(StorageError::Failed(reason));
        }
        Ok(())
    }
}

impl<D: DriverIo> ResourceExt for Resource<D> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.check_health()?;

        let effective_len = {
            let final_len = self.inner.state.lock_sync().final_len;
            let storage_len = self.inner.driver.storage_len();
            let data_len = final_len.unwrap_or(storage_len);
            data_len.min(storage_len)
        };

        if offset >= effective_len {
            return Ok(0);
        }

        // Clamp buf to effective_len.
        #[expect(clippy::cast_possible_truncation)] // byte offset within allocated resource
        let available = (effective_len - offset) as usize;
        let to_read = buf.len().min(available);

        self.inner
            .driver
            .read_at(offset, &mut buf[..to_read], effective_len)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
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

        let committed = {
            let state = self.inner.state.lock_sync();
            state.committed
        };

        self.inner.driver.write_at(offset, data, committed)?;

        // Notify fast-path (mmap: push to lock-free queue).
        let range = offset..end;
        self.inner.driver.notify_write(&range);

        // Update common state and wake waiters.
        {
            let mut state = self.inner.state.lock_sync();
            state.available.insert(range);

            // Invalidate evicted ranges for ring buffer drivers.
            if let Some(window) = self.inner.driver.valid_window() {
                if window.start > 0 {
                    state.available.remove(0..window.start);
                }
                // Also evict data above window end (backward seek case).
                // For ring buffers, data outside the valid window is overwritten.
                let upper = state.final_len.unwrap_or(u64::MAX);
                if window.end < upper {
                    state.available.remove(window.end..upper);
                }
            }
        }
        self.inner.condvar.notify_all();

        Ok(())
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara_hang_detector::hang_watchdog]
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

        let mut prev_available_len: u64 = 0;
        loop {
            // Fast path: let the driver check without holding state lock.
            if self.inner.driver.try_fast_check(&range) {
                return Ok(WaitOutcome::Ready);
            }

            // Slow path: lock state and check coverage, wait if needed.
            let state = self.inner.state.lock_sync();

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
                // Clamp range to file size: readers may request beyond
                // EOF (e.g., Symphonia probing). Data within 0..final_len
                // is what matters.
                let clamped = range.start..range.end.min(final_len);
                if range_covered_by(&state.available, &clamped) {
                    return Ok(WaitOutcome::Ready);
                }
                // For non-ring-buffer drivers, committed means all data
                // is available (range_covered_by above may fail if
                // available wasn't populated, but the data is on disk).
                if self.inner.driver.valid_window().is_none() {
                    return Ok(WaitOutcome::Ready);
                }
                // Ring buffer with evicted data: fall through to
                // spin-wait. The on-demand mechanism re-downloads
                // needed data and notifies the condvar.
            }

            // Reset hang detector when download makes progress.
            let current_available_len: u64 = state.available.iter().map(|r| r.end - r.start).sum();
            if current_available_len > prev_available_len {
                prev_available_len = current_available_len;
                hang_reset!();
            }

            debug!(
                range_start = range.start,
                range_end = range.end,
                committed = state.committed,
                final_len = ?state.final_len,
                "storage::wait_range spinning"
            );

            hang_tick!();
            yield_now();
            let deadline = Instant::now() + PlatformDuration::from_millis(WAIT_SPIN_TIMEOUT_MS);
            let (_state, _wait_result) = self.inner.condvar.wait_sync_timeout(state, deadline);
        }
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;

        // Driver-specific commit first (may fail).
        self.inner.driver.commit(final_len)?;

        // Update common state only on success.
        {
            let mut state = self.inner.state.lock_sync();
            state.committed = true;
            state.final_len = final_len;
            if let Some(len) = final_len
                && len > 0
            {
                state.available.insert(0..len);
                // For ring buffer drivers, only data in the valid window is
                // actually available. Remove evicted ranges.
                if let Some(window) = self.inner.driver.valid_window() {
                    if window.start > 0 {
                        state.available.remove(0..window.start);
                    }
                    if window.end < len {
                        state.available.remove(window.end..len);
                    }
                }
            }
        }
        self.inner.condvar.notify_all();
        Ok(())
    }

    fn fail(&self, reason: String) {
        {
            let mut state = self.inner.state.lock_sync();
            state.failed = Some(reason);
        }
        self.inner.condvar.notify_all();
    }

    fn path(&self) -> Option<&Path> {
        self.inner.driver.path()
    }

    fn len(&self) -> Option<u64> {
        let state = self.inner.state.lock_sync();
        state.final_len
    }

    fn status(&self) -> ResourceStatus {
        let state = self.inner.state.lock_sync();
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

    fn contains_range(&self, range: Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        let state = self.inner.state.lock_sync();
        range_covered_by(&state.available, &range)
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        let state = self.inner.state.lock_sync();
        let total = state.final_len.unwrap_or(limit);
        let upper = limit.min(total);
        if from >= upper {
            return None;
        }
        state
            .available
            .gaps(&(from..upper))
            .next()
            .map(|gap| gap.start..gap.end.min(upper))
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.check_health()?;

        // Driver-specific reactivation first (may fail).
        self.inner.driver.reactivate()?;

        // Update common state only on success.
        {
            let mut state = self.inner.state.lock_sync();
            state.committed = false;
            state.final_len = None;
            // Keep available data as-is — existing bytes remain readable.
        }
        self.inner.condvar.notify_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test]
    fn driver_io_mock_api_is_generated() {
        let _ = DriverIoMock::read_at;
    }
}
