#![forbid(unsafe_code)]

//! `Resource<D>` struct, internal `Inner`/`CommonState`, constructors,
//! and the shared `check_health` helper. The actual `*_inner` method
//! bodies live in sibling modules (`io`, `wait`, `lifecycle`); the
//! [`ResourceExt`](crate::ResourceExt) trait wiring lives in `ops`.

use std::{
    fmt::{self, Debug},
    sync::Arc,
};

use derivative::Derivative;
use kithara_platform::{Condvar, Mutex};
use rangemap::RangeSet;
use tokio_util::sync::CancellationToken;

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
    pub(super) cancel: CancellationToken,
    pub(super) condvar: Condvar,
    pub(super) driver: D,
    pub(super) state: Mutex<CommonState>,
    pub(super) observer: Option<Arc<dyn AvailabilityObserver>>,
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
    pub(super) inner: Arc<Inner<D>>,
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
    pub fn open(cancel: CancellationToken, opts: D::Options) -> StorageResult<Self> {
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
    pub fn open_with_observer(
        cancel: CancellationToken,
        opts: D::Options,
        observer: Option<Arc<dyn AvailabilityObserver>>,
    ) -> StorageResult<Self> {
        let (driver, init) = D::open(opts)?;
        Ok(Self {
            inner: Arc::new(Inner {
                cancel,
                driver,
                observer,
                condvar: Condvar::new(),
                state: Mutex::new(CommonState {
                    failed: None,
                    final_len: init.final_len,
                    available: init.available,
                    committed: init.committed,
                }),
            }),
        })
    }
}

impl<D: DriverIo> Resource<D> {
    /// Check if the resource is cancelled or failed, returning error if so.
    pub(super) fn check_health(&self) -> StorageResult<()> {
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
