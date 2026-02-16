#![forbid(unsafe_code)]

//! Generic `Resource<D>` and `Driver` trait.
//!
//! `Driver` abstracts backend-specific storage I/O (mmap, in-memory, etc.).
//! `Resource<D>` owns the common state machine (range tracking, committed/failed
//! flags, condvar coordination, cancellation) and delegates I/O to the driver.

use std::{fmt::Debug, ops::Range, path::Path, sync::Arc};

use kithara_platform::{Condvar, Mutex};
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;

use crate::{
    StorageError, StorageResult,
    resource::{ResourceExt, ResourceStatus, WaitOutcome, range_covered_by},
};

/// Backend-specific storage operations.
///
/// Drivers handle raw byte I/O and storage lifecycle transitions.
/// The common state machine (range tracking, committed/failed flags,
/// condvar coordination, cancellation) is managed by [`Resource<D>`].
///
/// Each driver manages its own interior mutability (e.g. `Mutex<MmapState>`
/// for mmap, `Mutex<Vec<u8>>` for memory).
pub trait Driver: Send + Sync + 'static {
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

/// Initial state returned by [`Driver::open`] to populate [`Resource<D>`].
pub struct DriverState {
    /// Pre-populated available byte ranges.
    pub available: RangeSet<u64>,
    /// Whether the resource starts as committed.
    pub committed: bool,
    /// Known final length (if committed).
    pub final_len: Option<u64>,
}

impl Default for DriverState {
    fn default() -> Self {
        Self {
            available: RangeSet::new(),
            committed: false,
            final_len: None,
        }
    }
}

/// Common state tracked by `Resource<D>`.
struct CommonState {
    available: RangeSet<u64>,
    committed: bool,
    final_len: Option<u64>,
    failed: Option<String>,
}

/// Shared inner storage.
struct Inner<D: Driver> {
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
pub struct Resource<D: Driver> {
    inner: Arc<Inner<D>>,
}

impl<D: Driver> Clone for Resource<D> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<D: Driver + Debug> Debug for Resource<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock();
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

    /// Check if the resource is cancelled or failed, returning error if so.
    fn check_health(&self) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(StorageError::Cancelled);
        }
        let failed = {
            let state = self.inner.state.lock();
            state.failed.clone()
        };
        if let Some(reason) = failed {
            return Err(StorageError::Failed(reason));
        }
        Ok(())
    }
}

impl<D: Driver> ResourceExt for Resource<D> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.check_health()?;

        let effective_len = {
            let final_len = self.inner.state.lock().final_len;
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
            let state = self.inner.state.lock();
            state.committed
        };

        self.inner.driver.write_at(offset, data, committed)?;

        // Notify fast-path (mmap: push to lock-free queue).
        let range = offset..end;
        self.inner.driver.notify_write(&range);

        // Update common state and wake waiters.
        {
            let mut state = self.inner.state.lock();
            state.available.insert(range);

            // Invalidate evicted ranges for ring buffer drivers.
            if let Some(window) = self.inner.driver.valid_window() {
                let evict_end = window.start;
                if evict_end > 0 {
                    state.available.remove(0..evict_end);
                }
            }
        }
        self.inner.condvar.notify_all();

        Ok(())
    }

    #[expect(clippy::significant_drop_tightening)] // lock must be held for condvar.wait_for
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

        loop {
            // Fast path: let the driver check without holding state lock.
            if self.inner.driver.try_fast_check(&range) {
                return Ok(WaitOutcome::Ready);
            }

            // Slow path: lock state and check coverage, wait if needed.
            let mut state = self.inner.state.lock();

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

            // On wasm32, parking_lot's wait_for() calls std::time::Instant::now()
            // which panics ("time not implemented"). Use untimed wait() instead.
            #[cfg(not(target_arch = "wasm32"))]
            self.inner
                .condvar
                .wait_for(&mut state, std::time::Duration::from_millis(50));
            #[cfg(target_arch = "wasm32")]
            self.inner.condvar.wait(&mut state);
        }
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;

        // Driver-specific commit first (may fail).
        self.inner.driver.commit(final_len)?;

        // Update common state only on success.
        {
            let mut state = self.inner.state.lock();
            state.committed = true;
            state.final_len = final_len;
            if let Some(len) = final_len
                && len > 0
            {
                state.available.insert(0..len);
            }
        }
        self.inner.condvar.notify_all();
        Ok(())
    }

    fn fail(&self, reason: String) {
        {
            let mut state = self.inner.state.lock();
            state.failed = Some(reason);
        }
        self.inner.condvar.notify_all();
    }

    fn path(&self) -> Option<&Path> {
        self.inner.driver.path()
    }

    fn len(&self) -> Option<u64> {
        let state = self.inner.state.lock();
        state.final_len
    }

    fn status(&self) -> ResourceStatus {
        let state = self.inner.state.lock();
        #[expect(clippy::option_if_let_else)] // three-way branch is clearer with if-let-else
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

    fn reactivate(&self) -> StorageResult<()> {
        self.check_health()?;

        // Driver-specific reactivation first (may fail).
        self.inner.driver.reactivate()?;

        // Update common state only on success.
        {
            let mut state = self.inner.state.lock();
            state.committed = false;
            state.final_len = None;
            // Keep available data as-is — existing bytes remain readable.
        }
        self.inner.condvar.notify_all();
        Ok(())
    }
}
