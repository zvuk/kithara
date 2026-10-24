#![forbid(unsafe_code)]

use std::{ops::Range, path::Path};

use kithara_platform::{
    CancelToken,
    maybe_send::{MaybeSend, MaybeSync},
    sync::Arc,
};

pub use super::phase::{Active, Committed, Reader, ResourcePhase};
use super::{
    phase::{ReadCore, WriteGuard},
    sealed,
    state::ResourceCore,
};
use crate::{
    StorageResult,
    backend::traits::{AvailabilityObserver, Driver, DriverIo},
    resource::{ResourceStatus, WaitOutcome},
};

/// Phantom-typestate storage resource. The phase `S` selects the stored payload
/// and the available methods:
/// - [`Resource<Active, D>`] (alias [`ResourceWriter`]) — `write_at`,
///   `write_all`, `fail`, `reader`, and consume-self `commit`.
/// - [`Resource<Committed, D>`] — `final_len`, `reader`, consume-self
///   `reactivate`.
/// - [`Resource<Reader, D>`] (alias [`ResourceReader`]) — cloneable read-only.
///
/// Read API for all three phases lives on the sealed [`ResourceRead`] trait.
pub struct Resource<S: ResourcePhase, D: DriverIo> {
    data: S::Data<D>,
}

/// The writable, single-owner resource handle.
pub type ResourceWriter<D> = Resource<Active, D>;
/// A cloneable read-only resource view.
pub type ResourceReader<D> = Resource<Reader, D>;

impl<D: Driver> Resource<Active, D> {
    /// Open a writable resource with no availability observer.
    ///
    /// # Errors
    /// Returns error if `D::open(opts)` fails.
    pub fn open(cancel: CancelToken, opts: D::Options) -> StorageResult<Self> {
        Ok(Self {
            data: WriteGuard {
                core: ResourceCore::open(cancel, opts)?,
            },
        })
    }

    /// Open a writable resource with an optional [`AvailabilityObserver`].
    ///
    /// # Errors
    /// Returns error if `D::open(opts)` fails.
    pub fn open_with_observer(
        cancel: CancelToken,
        opts: D::Options,
        observer: Option<Arc<dyn AvailabilityObserver>>,
    ) -> StorageResult<Self> {
        Ok(Self {
            data: WriteGuard {
                core: ResourceCore::open_with_observer(cancel, opts, observer)?,
            },
        })
    }
}

impl<D: DriverIo> Resource<Active, D> {
    /// Commit the resource, consuming the writer and returning the sealed
    /// `Committed` handle. A second `commit` (or any `write_at`) is a compile
    /// error: the writer value is moved out here.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the backend
    /// cannot finalize.
    pub fn commit(self, final_len: Option<u64>) -> StorageResult<Resource<Committed, D>> {
        self.data.core.commit_inner(final_len)?;
        let core = self.data.core.clone();
        Ok(Resource {
            data: ReadCore { core },
        })
    }

    /// Mint a cloneable read-only view over the in-flight resource (readers may
    /// observe the committed prefix while the writer keeps appending).
    #[must_use]
    pub fn reader(&self) -> Resource<Reader, D> {
        Resource {
            data: ReadCore {
                core: self.data.core.clone(),
            },
        }
    }

    /// Write entire contents and commit atomically, consuming the writer.
    ///
    /// # Errors
    /// Returns error if the write or commit fails.
    pub fn write_all(self, data: &[u8]) -> StorageResult<Resource<Committed, D>> {
        self.data.core.write_at_inner(0, data)?;
        self.commit(Some(data.len() as u64))
    }

    delegate::delegate! {
        to self.data.core {
            /// Commit without consuming, for a decorator that rewrites in place.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the backend
            /// cannot finalize.
            #[call(commit_inner)]
            pub(crate) fn commit_in_place(&self, final_len: Option<u64>) -> StorageResult<()>;
            /// Notify the original observer after publishing the canonical
            /// backing resource and releasing the decorator's handover lock.
            #[call(notify_commit_inner)]
            pub(crate) fn notify_commit_in_place(&self, final_len: Option<u64>);
            /// Stage a reopened readable snapshot until its original observer
            /// has recorded the commit, without reactivating its backing store.
            #[call(stage_commit_inner)]
            pub(crate) fn stage_commit_in_place(&self);
            /// Publish readiness and wake waiters after the original observer
            /// has recorded the successfully published backing resource.
            #[call(publish_commit_inner)]
            pub(crate) fn publish_commit_in_place(&self, final_len: Option<u64>);
            /// Release this writer without failing the shared core or poisoning a successor's writes.
            /// Use when the caller owns re-dispatch after cancellation; ordinary uncommitted drop fails the core.
            /// Readers remain waiting until that replacement supplies bytes.
            #[call(abandon_inner)]
            pub fn abandon(&self);
            /// Mark the resource as failed, consuming the writer.
            #[call(fail_inner)]
            pub fn fail(self, reason: String);
            /// Mark failed without consuming, for a decorator that owns the writer.
            #[call(fail_inner)]
            pub(crate) fn fail_in_place(&self, reason: String);
            /// Drop the driver's handles on its path, for a decorator that is
            /// about to replace the file underneath it.
            ///
            /// # Errors
            /// Returns error if pending bytes cannot be flushed first.
            #[call(release_backing_inner)]
            pub(crate) fn release_backing_in_place(&self, final_len: Option<u64>) -> StorageResult<()>;
            /// Reactivate without consuming, for a decorator that rewrites in place.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled or the backend cannot reopen.
            #[call(reactivate_inner)]
            pub(crate) fn reactivate_in_place(&self) -> StorageResult<()>;
            /// Write data at the given offset.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the write fails.
            #[call(write_at_inner)]
            pub fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }
}

impl<D: DriverIo> Resource<Committed, D> {
    /// Committed length, if known.
    #[must_use]
    pub fn final_len(&self) -> Option<u64> {
        self.data.core.len_inner()
    }

    /// Reactivate a committed resource for continued writing, consuming the
    /// committed handle and returning a fresh writer. Reactivating an `Active`
    /// handle is a compile error: no such method exists on the writer.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled or the backend cannot reopen.
    pub fn reactivate(self) -> StorageResult<Resource<Active, D>> {
        self.data.core.reactivate_inner()?;
        let ReadCore { core } = self.data;
        Ok(Resource {
            data: WriteGuard { core },
        })
    }

    /// Mint a cloneable read-only view.
    #[must_use]
    pub fn reader(&self) -> Resource<Reader, D> {
        Resource {
            data: ReadCore {
                core: self.data.core.clone(),
            },
        }
    }
}

impl<D: DriverIo> Clone for Resource<Reader, D> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
        }
    }
}

/// Sealed read API shared by every resource phase. Implemented by
/// [`Resource<Active, D>`], [`Resource<Committed, D>`] and
/// [`Resource<Reader, D>`].
pub trait ResourceRead: sealed::Sealed + MaybeSend + MaybeSync {
    /// Whether the given range is fully covered by available data (non-blocking).
    fn contains_range(&self, range: Range<u64>) -> bool;

    /// Returns `true` if the resource has been committed with zero length.
    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Committed length, if known.
    fn len(&self) -> Option<u64>;

    /// First gap in available data starting at `from`, up to `limit`.
    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;

    /// Backing file path, if any.
    fn path(&self) -> Option<&Path>;

    /// Read data at the given offset into `buf`; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;

    /// Read the **active working storage**, bypassing the lock-free committed
    /// snapshot. A re-download keeps the prior generation's snapshot published
    /// for concurrent [`read_at`](Self::read_at) callers; the producer's own
    /// in-flight read-back (e.g. decrypt on commit) reads here to observe the
    /// freshly-written generation rather than the stale snapshot.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        let Some(len) = self.len() else {
            let mut probe = [0u8; 1];
            let _ = self.read_at(0, &mut probe)?;
            return Ok(0);
        };
        if len == 0 {
            buf.clear();
            return Ok(0);
        }
        let len_usize = usize::try_from(len).unwrap_or(usize::MAX);
        buf.resize(len_usize, 0);
        let n = self.read_at(0, buf)?;
        buf.truncate(n);
        Ok(n)
    }

    /// Runtime lifecycle observation (the resource mutates under concurrent
    /// readers, so this is genuinely runtime state, not a type fact).
    fn status(&self) -> ResourceStatus;

    /// Wait until the given byte range is available.
    ///
    /// # Errors
    /// Returns error if the range is invalid, the resource is cancelled, or the
    /// resource has failed.
    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;

    /// Wait until the given byte range is available, interrupting only this
    /// wait when `cancel` fires.
    ///
    /// # Errors
    /// Returns error if the range is invalid, either cancellation token fires,
    /// or the resource has failed.
    fn wait_range_with_cancel(
        &self,
        range: Range<u64>,
        cancel: &CancelToken,
    ) -> StorageResult<WaitOutcome>;
}

impl<S: ResourcePhase, D: DriverIo> sealed::Sealed for Resource<S, D> {}

macro_rules! impl_resource_read {
    ($($phase:ty),+ $(,)?) => {$(
        impl<D: DriverIo> ResourceRead for Resource<$phase, D> {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
                self.data.core.read_at_inner(offset, buf)
            }
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
                self.data.core.read_inflight_at(offset, buf)
            }
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
                self.data.core.wait_range_inner(range, None)
            }
            fn wait_range_with_cancel(
                &self,
                range: Range<u64>,
                cancel: &CancelToken,
            ) -> StorageResult<WaitOutcome> {
                self.data.core.wait_range_inner(range, Some(cancel))
            }
            fn len(&self) -> Option<u64> {
                self.data.core.len_inner()
            }
            fn path(&self) -> Option<&Path> {
                self.data.core.path_inner()
            }
            fn contains_range(&self, range: Range<u64>) -> bool {
                self.data.core.contains_range_inner(range)
            }
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
                self.data.core.next_gap_inner(from, limit)
            }
            fn status(&self) -> ResourceStatus {
                self.data.core.status_inner()
            }
        }
    )+};
}

impl_resource_read!(Active, Committed, Reader);
