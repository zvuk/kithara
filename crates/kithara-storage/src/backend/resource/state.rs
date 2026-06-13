#![forbid(unsafe_code)]

use std::{
    fmt::{self, Debug},
    sync::{Arc, atomic::AtomicBool},
};

use kithara_platform::{
    CancelToken,
    sync::{Condvar, Mutex},
};
use rangemap::RangeSet;

use crate::{
    StorageError, StorageResult,
    backend::traits::{AvailabilityObserver, Driver, DriverIo},
};

/// Common state tracked by `Resource<D>`.
pub(super) struct CommonState {
    pub(super) failed: Option<String>,
    pub(super) final_len: Option<u64>,
    pub(super) available: RangeSet<u64>,
    pub(super) committed: bool,
}

/// Shared inner storage.
pub(super) struct Inner<D: DriverIo> {
    pub(super) cancel: CancelToken,
    pub(super) condvar: Condvar,
    pub(super) driver: D,
    pub(super) state: Mutex<CommonState>,
    pub(super) observer: Option<Arc<dyn AvailabilityObserver>>,
    /// Lock-free lifecycle flag: `true` while the resource is committed, `false`
    /// once `reactivate` reopens it for a re-download. Distinct from the driver's
    /// committed snapshot, which stays published across a reactivate so reads
    /// keep serving consistent (immutable) bytes; this flag tracks the *lifecycle*
    /// so `len()` reports `None` for an active (being-rewritten) resource without
    /// taking the state mutex.
    pub(super) committed: AtomicBool,
}

/// Generic storage resource state machine, parameterized by backend driver.
///
/// Owns the common state machine (range tracking, committed/failed flags,
/// condvar wait coordination, cancellation) and delegates backend-specific
/// I/O to `D`. This is the shared backing for the typed handles
/// [`Resource<Active, D>`](crate::Resource), [`Resource<Committed, D>`] and
/// [`ReadHandle<D>`](crate::ReadHandle); it is crate-internal and never exposed
/// directly.
pub(crate) struct ResourceCore<D: DriverIo> {
    pub(super) inner: Arc<Inner<D>>,
}

impl<D: DriverIo> Clone for ResourceCore<D> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<D: DriverIo + Debug> Debug for ResourceCore<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state.lock();
        f.debug_struct("ResourceCore")
            .field("driver", &self.inner.driver)
            .field("committed", &state.committed)
            .field("final_len", &state.final_len)
            .finish()
    }
}

impl<D: Driver> ResourceCore<D> {
    /// Open a resource from driver options with no availability observer.
    ///
    /// `D` is resolved at the call site by the type of `opts` (e.g.
    /// `Resource::open(token, MmapOptions { .. })` resolves to
    /// `Resource<MmapDriver>::open`). The crate-private `Driver` trait
    /// drives the binding without exposing its `Options` associated type
    /// as a second canonical path to the public `MemOptions` / `MmapOptions`.
    ///
    /// # Errors
    ///
    /// Returns error if `D::open(opts)` fails (e.g. file I/O failure).
    pub(crate) fn open(cancel: CancelToken, opts: D::Options) -> StorageResult<Self> {
        Self::open_with_observer(cancel, opts, None)
    }

    /// Open a resource with an optional [`AvailabilityObserver`]. The
    /// observer (if any) fires after every successful `write_at` and
    /// after a successful `commit` that supplies a final length. Hooks
    /// run after the state lock is released.
    ///
    /// # Errors
    ///
    /// Returns error if `D::open(opts)` fails.
    pub(crate) fn open_with_observer(
        cancel: CancelToken,
        opts: D::Options,
        observer: Option<Arc<dyn AvailabilityObserver>>,
    ) -> StorageResult<Self> {
        let (driver, init) = D::open(opts)?;
        Ok(Self {
            inner: Arc::new(Inner {
                cancel,
                driver,
                observer,
                condvar: Condvar::default(),
                committed: AtomicBool::new(init.is_committed),
                state: Mutex::new(CommonState {
                    failed: None,
                    final_len: init.final_len,
                    available: init.available,
                    committed: init.is_committed,
                }),
            }),
        })
    }
}

impl<D: DriverIo> ResourceCore<D> {
    /// Check if the resource is cancelled or failed, returning error if so.
    pub(super) fn check_health(&self) -> StorageResult<()> {
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
