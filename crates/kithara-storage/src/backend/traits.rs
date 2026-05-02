#![forbid(unsafe_code)]

//! Backend driver contracts.
//!
//! [`DriverIo`] abstracts backend-specific storage I/O (mmap, in-memory, etc.).
//! Concrete drivers expose an inherent `open(opts)` returning `(Self,
//! DriverState)`; the generic [`Resource<D>`](crate::backend::Resource)
//! state machine then wraps the driver into a usable resource via
//! per-driver inherent ctors (e.g. `Resource::<MmapDriver>::open`).
//!
//! [`AvailabilityObserver`] receives byte-availability notifications.

use std::{ops::Range, path::Path};

use rangemap::RangeSet;

use crate::StorageResult;

/// Backend-specific storage operations.
///
/// Drivers handle raw byte I/O and storage lifecycle transitions.
/// The common state machine (range tracking, committed/failed flags,
/// condvar coordination, cancellation) is managed by
/// [`Resource<D>`](crate::backend::Resource).
///
/// Each driver manages its own interior mutability (e.g. `Mutex<MmapState>`
/// for mmap, `Mutex<Vec<u8>>` for memory).
#[cfg_attr(any(test, feature = "test-utils"), unimock::unimock(api = DriverIoMock))]
pub trait DriverIo: Send + Sync + 'static {
    /// Finalize backing store (e.g. mmap: resize + reopen read-only; memory: truncate).
    ///
    /// # Errors
    ///
    /// Returns error if the backing store cannot be finalized (e.g. file
    /// truncation or read-only reopen fails).
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;

    /// Publish a write range to the fast-path mechanism.
    ///
    /// Called by [`Resource<D>`](crate::backend::Resource) after a
    /// successful driver write, before updating state + condvar.
    fn notify_write(&self, _range: &Range<u64>) {}

    /// Filesystem path, if any.
    fn path(&self) -> Option<&Path>;

    /// Reopen for writing (e.g. mmap: reopen as read-write; memory: no-op).
    ///
    /// # Errors
    ///
    /// Returns error if the backing store cannot be reopened for writing.
    fn reactivate(&self) -> StorageResult<()>;

    /// Read bytes at offset into `buf`.
    ///
    /// `effective_len` is the min of `final_len` and physical storage length,
    /// pre-computed by [`Resource<D>`](crate::backend::Resource). The driver
    /// reads up to `effective_len`.
    ///
    /// # Errors
    ///
    /// Returns error if the underlying storage read fails.
    fn read_at(&self, offset: u64, buf: &mut [u8], effective_len: u64) -> StorageResult<usize>;

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

    /// Returns the valid data window for ring buffer drivers.
    ///
    /// When `Some(window)`, any ranges before `window.start` have been evicted
    /// and should be removed from the available set. Returns `None` for
    /// linear drivers where all written data is retained.
    fn valid_window(&self) -> Option<Range<u64>> {
        None
    }

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
}

/// Initial state returned by a driver's inherent `open` to populate
/// [`Resource<D>`](crate::backend::Resource).
///
/// Field order is (visibility, type, name): `Option` first, then the
/// `RangeSet`, then the boolean — keeps both this struct and its
/// initializer sites canonical.
#[derive(Default)]
pub struct DriverState {
    /// Known final length (if committed).
    pub final_len: Option<u64>,
    /// Pre-populated available byte ranges.
    pub available: RangeSet<u64>,
    /// Whether the resource starts as committed.
    pub committed: bool,
}

/// Driver factory + I/O contract.
///
/// `Driver` adds backend creation on top of the runtime [`DriverIo`]
/// operations. Public on purpose: the generic
/// `Resource::<D>::open(cancel, opts: D::Options)` constructor uses
/// `D::Options` as the call-site type-driver disambiguation knob — every
/// `Resource::open(token, MmapOptions { .. })` callsite resolves to
/// `Resource<MmapDriver>::open` because `MmapOptions = <MmapDriver as Driver>::Options`.
///
/// The `redundant_reexport` audit lint flags the dual surface
/// (`pub use mmap::MmapOptions` AND `<MmapDriver as Driver>::Options = MmapOptions`)
/// — the duplication is intentional: `MmapOptions` is the canonical
/// constructor type users reach via `kithara_storage::MmapOptions`, while
/// the trait associated type is the bound that lets `Resource::open`
/// stay generic. See `crates/kithara-storage/README.md` for the rationale.
pub trait Driver: DriverIo {
    /// Configuration needed to open/create a driver instance.
    type Options: Send;

    /// Open or create a new driver from options.
    ///
    /// Returns the driver and initial state to populate
    /// [`Resource<D>`](crate::backend::Resource).
    ///
    /// # Errors
    ///
    /// Returns error if the backing storage cannot be opened or created.
    fn open(opts: Self::Options) -> StorageResult<(Self, DriverState)>
    where
        Self: Sized;
}

/// Observer notified when a [`Resource<D>`](crate::backend::Resource) gains
/// bytes or finalizes.
///
/// Fires after the state lock is released so observer implementations
/// are free to take their own locks without risking deadlocks against
/// `wait_range` waiters. Storage knows nothing about resource keys —
/// implementations capture any attribution context themselves (see
/// `kithara-assets::index::ScopedAvailabilityObserver`).
pub trait AvailabilityObserver: Send + Sync {
    /// Record that the resource has been committed with `final_len`
    /// bytes. Only fires for `commit(Some(final_len))`; `commit(None)`
    /// is silent.
    fn on_commit(&self, final_len: u64);

    /// Record that `range` has just become available.
    fn on_write(&self, range: Range<u64>);
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
