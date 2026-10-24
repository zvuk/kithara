#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
use std::fs::TryLockError;
use std::{
    fs::{self, File, OpenOptions},
    ops::Range,
    path::{Path, PathBuf},
};

use arc_swap::ArcSwap;
#[cfg(not(target_arch = "wasm32"))]
use fs4::{FileExt, TryLockError};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, RwLock},
};

use crate::{
    ResourceRead, ResourceReader, ResourceStatus, ResourceWriter, StorageError, StorageResult,
    WaitOutcome, backend::traits::DriverIo,
};

/// Build the temp-file companion path for atomic chunked commits:
/// `segments/0001.bin` → `segments/0001.bin.tmp`. Sibling in the
/// same directory so `rename` is atomic on the same filesystem.
pub(super) fn make_tmp_path(canonical: &Path) -> Option<PathBuf> {
    let parent = canonical.parent()?;
    let name = canonical.file_name()?.to_str()?;
    Some(parent.join(format!("{name}.tmp")))
}

/// One writer's hold on `<canonical>.tmp` for the lifetime of its claim.
///
/// The claim is the *lock*, not the file. `file` holds an exclusive advisory
/// lock (`flock(LOCK_EX)`), which the OS releases when the handle closes —
/// including when the owning process dies. That is what separates a live
/// sibling writer from an orphan a `kill -9` left behind; the file's mere
/// existence cannot.
///
/// `fs4` supplies the lock: its `flock` reaches every non-wasm target, and
/// its `EWOULDBLOCK`/`EAGAIN` mapping is the `WouldBlock` the claim protocol
/// reads as "a live writer holds it".
struct TmpClaim {
    file: File,
    path: PathBuf,
}

impl TmpClaim {
    /// Claim the tmp exclusively, then truncate; a losing claimant must not touch live writer bytes.
    /// Stale bytes would otherwise be adopted as committed by mmap, and commit cannot trim them
    /// when final length is unknown. The lock, not the tmp's existence or size, proves ownership.
    fn take(path: PathBuf) -> StorageResult<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match Self::try_claim(&file) {
            Ok(()) => {
                file.set_len(0)?;
                Ok(Self { file, path })
            }
            Err(TryLockError::WouldBlock) => Err(StorageError::TmpClaimed(path)),
            Err(TryLockError::Error(e)) => Err(StorageError::Io(e)),
        }
    }

    /// The exclusive, non-blocking `flock` the claim is made of.
    #[cfg(not(target_arch = "wasm32"))]
    fn try_claim(file: &File) -> Result<(), TryLockError> {
        FileExt::try_lock(file)
    }

    /// The exclusive, non-blocking `flock` the claim is made of.
    #[cfg(target_arch = "wasm32")]
    fn try_claim(file: &File) -> Result<(), TryLockError> {
        file.try_lock()
    }
}

/// When the committed bytes are forced onto the medium.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Barrier {
    /// `sync_data` before the rename. The canonical path can then only ever
    /// appear fully durable, at the cost of one medium-speed wait per commit.
    Inline,
    /// No barrier here — the owner forces these files down itself and only
    /// then records them as usable. Until it does, the file on disk proves
    /// nothing, so the owner must not treat its mere existence as readiness.
    Deferred,
}

/// Hint passed to the factory closure to disambiguate the two
/// lifecycle calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenIntent {
    /// Initial open at the temp path: caller should produce a
    /// writable resource. The caller's `MmapOptions` should set
    /// `mode = OpenMode::ReadWrite`.
    Fresh,
    /// Reopen at the canonical path post-rename: caller should
    /// produce a read-only / already-committed resource so the
    /// resource's `status()` reports `Committed`. The caller's
    /// `MmapOptions` should set `mode = OpenMode::ReadOnly`.
    Reopen,
}

/// Open the tmp writer with [`OpenIntent::Fresh`], then the canonical file with Reopen after rename.
/// Reopen must return Committed status; otherwise a lease mistakes the renamed file for
/// an abandoned writer and deletes it.
type FactoryFn<D> =
    Box<dyn Fn(&Path, OpenIntent) -> StorageResult<ResourceWriter<D>> + Send + Sync>;

/// Decorator for crash-safe chunked writes over a single-owner
/// [`ResourceWriter`].
///
/// During the write phase the inner resource is mmapped at
/// `<canonical>.tmp`. On `commit()` the temp file is atomically renamed to
/// `canonical` and the inner is reopened there, so an external observer of
/// the canonical path either sees no file or sees the whole thing. Whether
/// those bytes are also *durable* by then is [`Barrier`]'s business.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, deref = false)]
pub struct AtomicChunked<D: DriverIo> {
    /// The current writer. Swapped (not cloned) on the commit-rename.
    /// Read/wait paths mint a cheap `ResourceReader` from the current snapshot.
    inner: ArcSwap<ResourceWriter<D>>,
    barrier: Barrier,
    /// `Some(claim on <path>.tmp)` while writes are in flight; cleared on
    /// successful `commit`. `Drop` / `fail` use a still-set value to remove
    /// the orphaned temp file. Dropping the claim releases its lock.
    claim: Mutex<Option<TmpClaim>>,
    /// Factory to reopen the inner on the canonical path post-rename.
    /// `None` when the wrapper was constructed in passthrough mode.
    factory: Option<FactoryFn<D>>,
    #[field(get(
        deref = Path,
        doc = "Path the resource will land at on a successful commit."
    ))]
    canonical_path: PathBuf,
    /// Held shared by every read that mints a view and uses it, and exclusively
    /// by `commit` from releasing the inner's mapping until the reopened inner
    /// is in place — so no read lands on a released inner. Blocking waits stay
    /// outside it: the commit they wait for needs it exclusively.
    handover: RwLock<()>,
}

impl<D: DriverIo> std::fmt::Debug for AtomicChunked<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tmp = self
            .claim
            .try_lock()
            .map(|g| g.as_ref().map(|claim| claim.path.clone()));
        f.debug_struct("AtomicChunked")
            .field("canonical_path", &self.canonical_path)
            .field("tmp_path", &tmp)
            .field("atomic", &self.factory.is_some())
            .finish_non_exhaustive()
    }
}

impl<D: DriverIo> AtomicChunked<D> {
    /// Release the writer without failing the resource, keeping the temp file.
    ///
    /// The caller owns the refill, so the partial bytes belong to the successor
    /// now — removing them, as [`fail`](Self::fail) does, would delete work in
    /// flight.
    pub fn abandon(&self) {
        self.inner.load().abandon();
    }

    /// Commit the accumulated chunks: durably flush + atomically rename the
    /// temp file to the canonical path + reopen the inner on the canonical
    /// path. Rewrites in place (the writer commits without being consumed).
    ///
    /// # Errors
    /// Propagates the inner commit error and any filesystem error.
    ///
    /// Releasing the backing flushes the final prefix without publishing a discarded snapshot.
    /// Windows requires that release before resizing or renaming the mapped file; the factory
    /// reopens on the canonical path.
    pub fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let Some(claim) = self.claim.lock().take() else {
            return self.inner.load().commit_in_place(final_len);
        };
        let TmpClaim { path: tmp, file } = &claim;

        let handover = self.handover.write();
        let inner = self.inner.load();
        let result: StorageResult<_> = (|| {
            inner.release_backing_in_place(final_len)?;

            if let Some(len) = final_len
                && file.metadata().is_ok_and(|m| m.len() > len)
            {
                file.set_len(len).map_err(|e| {
                    StorageError::Failed(format!("AtomicChunked commit: trim {tmp:?}: {e}"))
                })?;
            }
            if self.barrier == Barrier::Inline {
                file.sync_data().map_err(|e| {
                    StorageError::Failed(format!("AtomicChunked commit: sync_data {tmp:?}: {e}"))
                })?;
            }
            fs::rename(tmp, &self.canonical_path).map_err(|e| {
                StorageError::Failed(format!(
                    "AtomicChunked commit: rename {tmp:?} -> {:?}: {e}",
                    self.canonical_path
                ))
            })?;

            let new_inner = self
                .factory
                .as_ref()
                .map(|factory| factory(&self.canonical_path, OpenIntent::Reopen))
                .transpose()?;
            let new_inner = new_inner.map(|resource| {
                resource.stage_commit_in_place();
                Arc::new(resource)
            });
            if let Some(new_inner) = &new_inner {
                self.inner.store(Arc::clone(new_inner));
            }
            Ok(new_inner)
        })();
        if let Err(error) = &result {
            inner.fail_in_place(error.to_string());
        }
        drop(handover);
        let new_inner = result?;
        inner.notify_commit_in_place(final_len);
        if let Some(new_inner) = new_inner {
            new_inner.publish_commit_in_place(final_len);
        }
        inner.publish_commit_in_place(final_len);
        Ok(())
    }

    /// Whether the given range is fully covered by available data.
    pub fn contains_range(&self, range: Range<u64>) -> bool {
        self.read_settled(|view| view.contains_range(range))
    }

    /// Mark the resource failed and remove the orphaned temp file.
    pub fn fail(&self, reason: String) {
        self.inner.load().fail_in_place(reason);
        let claim = self.claim.lock().take();
        if let Some(claim) = claim {
            let _ = fs::remove_file(&claim.path);
        }
    }

    /// Committed length, if known.
    #[must_use]
    pub fn len(&self) -> Option<u64> {
        self.read_settled(ResourceRead::len)
    }

    /// First gap in available data starting at `from`, up to `limit`.
    pub fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.read_settled(|view| view.next_gap(from, limit))
    }

    /// Claim `<canonical>.tmp` with an exclusive advisory lock, reclaiming dead owners' tmp files.
    /// The factory opens Fresh at the tmp path and Reopen at the canonical path after commit rename.
    ///
    /// # Errors
    /// Returns [`StorageError::Failed`] for a missing parent or non-UTF8 filename,
    /// [`StorageError::TmpClaimed`] for a live writer, and OS/factory I/O or mmap errors.
    pub fn open<F>(canonical_path: PathBuf, factory: F) -> StorageResult<Self>
    where
        F: Fn(&Path, OpenIntent) -> StorageResult<ResourceWriter<D>> + Send + Sync + 'static,
    {
        Self::open_with_barrier(canonical_path, factory, Barrier::Inline)
    }

    /// Like [`Self::open`], but the caller owns the durability barrier — see
    /// [`Barrier::Deferred`] for what it takes on by doing so.
    ///
    /// # Errors
    ///
    /// Same as [`Self::open`].
    pub fn open_deferred<F>(canonical_path: PathBuf, factory: F) -> StorageResult<Self>
    where
        F: Fn(&Path, OpenIntent) -> StorageResult<ResourceWriter<D>> + Send + Sync + 'static,
    {
        Self::open_with_barrier(canonical_path, factory, Barrier::Deferred)
    }

    fn open_with_barrier<F>(
        canonical_path: PathBuf,
        factory: F,
        barrier: Barrier,
    ) -> StorageResult<Self>
    where
        F: Fn(&Path, OpenIntent) -> StorageResult<ResourceWriter<D>> + Send + Sync + 'static,
    {
        let tmp_path = make_tmp_path(&canonical_path).ok_or_else(|| {
            StorageError::Failed(format!(
                "AtomicChunked: cannot derive tmp path from {canonical_path:?}"
            ))
        })?;
        if let Some(parent) = tmp_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let claim = TmpClaim::take(tmp_path)?;
        let inner = factory(&claim.path, OpenIntent::Fresh)?;
        Ok(Self {
            barrier,
            canonical_path,
            inner: ArcSwap::from_pointee(inner),
            claim: Mutex::new(Some(claim)),
            handover: RwLock::default(),
            factory: Some(Box::new(factory)),
        })
    }

    /// Wrap an already-opened inner with no atomicity (pass-through).
    /// Used for memory-backed inners that have no filesystem to
    /// protect, or for re-opens of files that are already committed
    /// on disk.
    #[must_use]
    pub fn passthrough(inner: ResourceWriter<D>, canonical_path: PathBuf) -> Self {
        Self {
            canonical_path,
            inner: ArcSwap::from_pointee(inner),
            claim: Mutex::default(),
            handover: RwLock::default(),
            factory: None,
            barrier: Barrier::Inline,
        }
    }

    /// Backing file path (the canonical path the resource lands at).
    pub fn path(&self) -> Option<&Path> {
        Some(&self.canonical_path)
    }

    /// Read data at the given offset into `buf`.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.read_settled(|view| view.read_at(offset, buf))
    }

    /// Read the writer's own in-flight bytes from the active working storage,
    /// bypassing the committed snapshot. Used by decrypt-on-commit read-back so
    /// it transforms the freshly-written generation, not a prior snapshot kept
    /// for concurrent readers during a rewrite.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.read_settled(|view| view.read_inflight_at(offset, buf))
    }

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        self.read_settled(|view| view.read_into(buf))
    }

    /// Run a non-blocking read against a view that no commit is retiring.
    fn read_settled<R>(&self, read: impl FnOnce(&ResourceReader<D>) -> R) -> R {
        let _handover = self.handover.read();
        read(&self.read_view())
    }

    /// Current runtime status.
    pub fn status(&self) -> ResourceStatus {
        self.read_settled(ResourceRead::status)
    }

    /// Wait until the given byte range is available.
    ///
    /// # Errors
    /// Returns error if the range is invalid, the resource is cancelled, or the
    /// resource has failed.
    pub fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.read_view().wait_range(range)
    }

    /// Wait until the given byte range is available, interrupting only this
    /// wait when `cancel` fires.
    ///
    /// # Errors
    /// Returns error if the range is invalid, either cancellation token fires,
    /// or the resource has failed.
    pub fn wait_range_with_cancel(
        &self,
        range: Range<u64>,
        cancel: &CancelToken,
    ) -> StorageResult<WaitOutcome> {
        self.read_view().wait_range_with_cancel(range, cancel)
    }

    delegate::delegate! {
        to self {
            /// Returns `true` if the resource has been committed with zero length.
            #[must_use]
            #[expr($ == Some(0))]
            #[call(len)]
            pub fn is_empty(&self) -> bool;
        }
        to self.inner.load() {
            /// Reactivate the inner for continued writing.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled or the backend cannot reopen.
            #[call(reactivate_in_place)]
            pub fn reactivate(&self) -> StorageResult<()>;
            /// Mint a cheap read-only view without holding the inner lock during
            /// subsequent (possibly blocking) reads.
            #[call(reader)]
            fn read_view(&self) -> ResourceReader<D>;
            /// Write data at the given offset.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the write fails.
            pub fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }
}

impl<D: DriverIo> Drop for AtomicChunked<D> {
    /// Clean up the orphaned temp file when a writer is dropped
    /// without a successful commit. A `kill -9` skips `Drop` entirely; the
    /// OS then releases the claim's lock instead, and the next
    /// `AtomicChunked::open` over the same canonical path reclaims the
    /// stale temp.
    fn drop(&mut self) {
        let claim = self.claim.lock().take();
        if let Some(claim) = claim {
            let _ = fs::remove_file(&claim.path);
        }
    }
}
