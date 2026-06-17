#![forbid(unsafe_code)]

use std::{
    fs::{self, OpenOptions},
    io,
    ops::Range,
    path::{Path, PathBuf},
};

use kithara_platform::sync::Mutex;

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

/// Hint passed to the factory closure to disambiguate the two
/// lifecycle calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenIntent {
    /// Initial open at the temp path: caller should produce a
    /// writeable resource. The caller's `MmapOptions` should set
    /// `mode = OpenMode::ReadWrite`.
    Fresh,
    /// Reopen at the canonical path post-rename: caller should
    /// produce a read-only / already-committed resource so the
    /// resource's `status()` reports `Committed`. The caller's
    /// `MmapOptions` should set `mode = OpenMode::ReadOnly`.
    Reopen,
}

/// Factory used to (re)open the inner writer at a given path.
///
/// Called twice in the atomic-chunked lifecycle:
///   1. With [`OpenIntent::Fresh`] — at [`AtomicChunked::open`],
///      opens the inner mmap on the sibling tmp path so chunked
///      writes accumulate there.
///   2. With [`OpenIntent::Reopen`] — at [`AtomicChunked::commit`]
///      after the atomic rename, opens a fresh read-only inner mmap
///      on the canonical path (a writer handle whose backing file is
///      already committed). The caller MUST honour the intent and
///      produce a Committed-status resource, otherwise the wrapping
///      layer (`LeaseResource::drop`) will mistake the just-renamed
///      file for an abandoned writer and delete it.
type FactoryFn<D> =
    Box<dyn Fn(&Path, OpenIntent) -> StorageResult<ResourceWriter<D>> + Send + Sync>;

/// Decorator for crash-safe chunked writes over a single-owner
/// [`ResourceWriter`].
///
/// During the write phase the inner resource is mmapped at
/// `<canonical>.tmp`. On `commit()` the data is durably flushed
/// (`sync_data`), the temp file is atomically renamed to
/// `canonical`, and the inner is reopened on the canonical path —
/// guaranteeing that any external observer of the canonical path
/// either sees no file or sees the fully durable committed bytes.
pub struct AtomicChunked<D: DriverIo> {
    /// The current writer. Swapped (not cloned) on the commit-rename.
    /// Read/wait paths mint a cheap `ResourceReader` and release the
    /// lock before blocking.
    inner: Mutex<ResourceWriter<D>>,
    /// `Some(<path>.tmp)` while writes are in flight; cleared on
    /// successful `commit`. `Drop` / `fail` use a still-set value to
    /// remove the orphaned temp file.
    tmp_path: Mutex<Option<PathBuf>>,
    /// Factory to reopen the inner on the canonical path post-rename.
    /// `None` when the wrapper was constructed in passthrough mode.
    factory: Option<FactoryFn<D>>,
    canonical_path: PathBuf,
}

impl<D: DriverIo> std::fmt::Debug for AtomicChunked<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tmp = self.tmp_path.try_lock().map(|g| g.clone());
        f.debug_struct("AtomicChunked")
            .field("canonical_path", &self.canonical_path)
            .field("tmp_path", &tmp)
            .field("atomic", &self.factory.is_some())
            .finish_non_exhaustive()
    }
}

impl<D: DriverIo> AtomicChunked<D> {
    /// Path the resource will land at on a successful commit.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Commit the accumulated chunks: durably flush + atomically rename the
    /// temp file to the canonical path + reopen the inner on the canonical
    /// path. Rewrites in place (the writer commits without being consumed).
    ///
    /// # Errors
    /// Propagates the inner commit error and any filesystem error.
    pub fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let mut guard = self.inner.lock();
        guard.commit_in_place(final_len)?;

        let tmp = self.tmp_path.lock().take();
        if let Some(tmp) = tmp {
            let f = OpenOptions::new().write(true).open(&tmp).map_err(|e| {
                StorageError::Failed(format!("AtomicChunked commit: open tmp {tmp:?}: {e}"))
            })?;
            f.sync_data().map_err(|e| {
                StorageError::Failed(format!("AtomicChunked commit: sync_data {tmp:?}: {e}"))
            })?;
            drop(f);
            fs::rename(&tmp, &self.canonical_path).map_err(|e| {
                StorageError::Failed(format!(
                    "AtomicChunked commit: rename {tmp:?} -> {:?}: {e}",
                    self.canonical_path
                ))
            })?;

            if let Some(factory) = self.factory.as_ref() {
                let new_inner = factory(&self.canonical_path, OpenIntent::Reopen)?;
                *guard = new_inner;
            }
        }
        drop(guard);
        Ok(())
    }

    /// Whether the given range is fully covered by available data.
    pub fn contains_range(&self, range: Range<u64>) -> bool {
        self.read_view().contains_range(range)
    }

    /// Mark the resource failed and remove the orphaned temp file.
    pub fn fail(&self, reason: String) {
        self.inner.lock().fail_in_place(reason);
        if let Some(tmp) = self.tmp_path.lock().take() {
            let _ = fs::remove_file(&tmp);
        }
    }

    /// Returns `true` if the resource has been committed with zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Committed length, if known.
    #[must_use]
    pub fn len(&self) -> Option<u64> {
        self.read_view().len()
    }

    /// First gap in available data starting at `from`, up to `limit`.
    pub fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.read_view().next_gap(from, limit)
    }

    /// Open a fresh chunked-atomic resource at `canonical_path`.
    /// The provided `factory` opens the inner at a given filesystem
    /// path; it is called once with the temp path during this
    /// constructor and once more with the canonical path after the
    /// atomic rename in [`AtomicChunked::commit`].
    ///
    /// Atomically claims `<canonical>.tmp` via `OpenOptions::create_new`
    /// — the filesystem rejects the second concurrent open of the same
    /// tmp path. Returns [`StorageError::TmpClaimed`] if another
    /// `AssetStore` instance (or another process) is already writing
    /// the same canonical path.
    ///
    /// # Errors
    ///
    /// - [`StorageError::Failed`] — canonical path has no parent /
    ///   non-utf8 file name.
    /// - [`StorageError::TmpClaimed`] — tmp path already exists.
    /// - [`StorageError::Io`] / [`StorageError::Mmap`] — propagated
    ///   from the OS or from the supplied factory.
    pub fn open<F>(canonical_path: PathBuf, factory: F) -> StorageResult<Self>
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
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => {
                drop(file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(StorageError::TmpClaimed(tmp_path));
            }
            Err(e) => return Err(StorageError::Io(e)),
        }
        let inner = factory(&tmp_path, OpenIntent::Fresh)?;
        Ok(Self {
            canonical_path,
            inner: Mutex::new(inner),
            tmp_path: Mutex::new(Some(tmp_path)),
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
            inner: Mutex::new(inner),
            tmp_path: Mutex::default(),
            factory: None,
        }
    }

    /// Backing file path (the canonical path the resource lands at).
    pub fn path(&self) -> Option<&Path> {
        Some(&self.canonical_path)
    }

    /// Reactivate the inner for continued writing.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled or the backend cannot reopen.
    pub fn reactivate(&self) -> StorageResult<()> {
        self.inner.lock().reactivate_in_place()
    }

    /// Read data at the given offset into `buf`.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.read_view().read_at(offset, buf)
    }

    /// Read the writer's own in-flight bytes from the active working storage,
    /// bypassing the committed snapshot. Used by decrypt-on-commit read-back so
    /// it transforms the freshly-written generation, not a prior snapshot kept
    /// for concurrent readers during a rewrite.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.read_view().read_inflight_at(offset, buf)
    }

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        self.read_view().read_into(buf)
    }

    /// Mint a cheap read-only view without holding the inner lock during
    /// subsequent (possibly blocking) reads.
    fn read_view(&self) -> ResourceReader<D> {
        self.inner.lock().reader()
    }

    /// Current runtime status.
    pub fn status(&self) -> ResourceStatus {
        self.read_view().status()
    }

    /// Wait until the given byte range is available.
    ///
    /// # Errors
    /// Returns error if the range is invalid, the resource is cancelled, or the
    /// resource has failed.
    pub fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.read_view().wait_range(range)
    }

    /// Write data at the given offset.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the write fails.
    pub fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.lock().write_at(offset, data)
    }
}

impl<D: DriverIo> Drop for AtomicChunked<D> {
    /// Clean up the orphaned temp file when a writer is dropped
    /// without a successful commit. Best-effort: a `kill -9` skips
    /// `Drop` entirely, in which case the next `AtomicChunked::open`
    /// over the same canonical path wipes the stale temp.
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp_path.lock().take() {
            let _ = fs::remove_file(&tmp);
        }
    }
}
