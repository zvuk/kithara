#![forbid(unsafe_code)]

use std::{ops::Range, path::Path};

use kithara_platform::{
    CancelToken,
    maybe_send::{MaybeSend, MaybeSync},
    sync::Arc,
};
use tracing::warn;

use super::state::ResourceCore;
use crate::{
    StorageResult,
    backend::traits::{AvailabilityObserver, Driver, DriverIo},
    resource::{ResourceStatus, WaitOutcome},
};

mod sealed {
    pub trait Sealed {}
}

/// Lifecycle phase of a [`Resource`]. Sealed: only the in-crate markers
/// [`Active`], [`Committed`] and [`Reader`] implement it.
pub trait ResourcePhase: sealed::Sealed {
    /// The per-phase payload stored inside `Resource<Self, D>`.
    type Data<D: DriverIo>;
}

/// Writeable, in-flight phase: single-owner, not `Clone`.
pub struct Active;
/// Sealed, fully-written phase: read-final, reactivatable.
pub struct Committed;
/// Cloneable read-only view minted from an `Active` or `Committed` handle.
pub struct Reader;

impl sealed::Sealed for Active {}
impl sealed::Sealed for Committed {}
impl sealed::Sealed for Reader {}

impl ResourcePhase for Active {
    type Data<D: DriverIo> = WriteGuard<D>;
}
impl ResourcePhase for Committed {
    type Data<D: DriverIo> = ReadCore<D>;
}
impl ResourcePhase for Reader {
    type Data<D: DriverIo> = ReadCore<D>;
}

/// Drop-guard payload for the `Active` writer phase. Owns the shared
/// `ResourceCore` and, if dropped without a successful `commit`/`fail`, marks
/// the core failed and wakes blocked readers. The `Drop` lives on this concrete
/// payload (full-generic match) so the phantom `Resource<S, D>` needs no
/// (illegal) specialized `Drop` impl.
pub struct WriteGuard<D: DriverIo> {
    core: ResourceCore<D>,
}

impl<D: DriverIo> Drop for WriteGuard<D> {
    fn drop(&mut self) {
        if self.core.should_fail_on_drop() {
            if let Some(path) = self.core.path_inner() {
                warn!(resource = ?path, "active resource writer dropped without commit");
            }
            self.core
                .fail_inner("active resource writer dropped without commit".to_owned());
        }
    }
}

/// Read-only payload shared by the `Committed` and `Reader` phases.
pub struct ReadCore<D: DriverIo> {
    core: ResourceCore<D>,
}

impl<D: DriverIo> Clone for ReadCore<D> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
        }
    }
}

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

/// The writeable, single-owner resource handle.
pub type ResourceWriter<D> = Resource<Active, D>;
/// A cloneable read-only resource view.
pub type ResourceReader<D> = Resource<Reader, D>;

impl<D: Driver> Resource<Active, D> {
    /// Open a writeable resource with no availability observer.
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

    /// Open a writeable resource with an optional [`AvailabilityObserver`].
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
        // `commit_inner` set `committed = true`, so the `WriteGuard` drop below
        // is a no-op. Clone the cheap `Arc`-backed core into the sealed handle.
        let core = self.data.core.clone();
        Ok(Resource {
            data: ReadCore { core },
        })
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
            /// Mark the resource as failed, consuming the writer.
            #[call(fail_inner)]
            pub fn fail(self, reason: String);
            /// Mark failed without consuming, for a decorator that owns the writer.
            #[call(fail_inner)]
            pub(crate) fn fail_in_place(&self, reason: String);
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
                self.data.core.wait_range_inner(range)
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
