#![forbid(unsafe_code)]

use std::{
    fs::{self, OpenOptions},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use kithara_platform::Mutex;

use crate::{
    StorageError, StorageResult,
    resource::{ResourceExt, ResourceStatus, WaitOutcome},
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

/// Factory used to (re)open the inner resource at a given path.
///
/// Called twice in the atomic-chunked lifecycle:
///   1. With [`OpenIntent::Fresh`] — at [`AtomicChunked::open`],
///      opens the inner mmap on the sibling tmp path so chunked
///      writes accumulate there.
///   2. With [`OpenIntent::Reopen`] — at [`ResourceExt::commit`]
///      after the atomic rename, opens a fresh read-only inner mmap
///      on the canonical path. The caller MUST honour the intent and
///      produce a Committed-status resource, otherwise the wrapping
///      layer (`LeaseResource::drop`) will mistake the just-renamed
///      file for an abandoned writer and delete it.
type FactoryFn<R> = Box<dyn Fn(&Path, OpenIntent) -> StorageResult<R> + Send + Sync>;

/// Decorator for crash-safe chunked writes.
///
/// During the write phase the inner resource is mmapped at
/// `<canonical>.tmp`. On `commit()` the data is durably flushed
/// (`sync_data`), the temp file is atomically renamed to
/// `canonical`, and the inner is reopened on the canonical path —
/// guaranteeing that any external observer of the canonical path
/// either sees no file or sees the fully durable committed bytes,
/// and that the inner's internal fs operations all target the file
/// that actually exists on disk.
pub struct AtomicChunked<R: ResourceExt> {
    /// Arc'd so we can clone-and-call without holding the outer
    /// mutex during blocking ops (e.g. `wait_range`). The mutex is
    /// only held briefly to clone the Arc or to swap the inner on
    /// commit-rename.
    inner: Mutex<Arc<R>>,
    /// `Some(<path>.tmp)` while writes are in flight; cleared on
    /// successful `commit`. `Drop` / `fail` use a still-set value to
    /// remove the orphaned temp file.
    tmp_path: Mutex<Option<PathBuf>>,
    /// Factory to reopen the inner on the canonical path post-rename.
    /// `None` when the wrapper was constructed in passthrough mode
    /// (no atomic rename to perform, no reopen needed).
    factory: Option<FactoryFn<R>>,
    canonical_path: PathBuf,
}

impl<R: ResourceExt> std::fmt::Debug for AtomicChunked<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tmp = self.tmp_path.try_lock().map(|g| g.clone());
        f.debug_struct("AtomicChunked")
            .field("canonical_path", &self.canonical_path)
            .field("tmp_path", &tmp)
            .field("atomic", &self.factory.is_some())
            .finish_non_exhaustive()
    }
}

impl<R: ResourceExt> AtomicChunked<R> {
    /// Path the resource will land at on a successful commit.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Clone the inner Arc — caller can call any `&self` method on
    /// the returned handle without holding the outer mutex.
    fn inner_clone(&self) -> Arc<R> {
        Arc::clone(&self.inner.lock_sync())
    }

    /// Open a fresh chunked-atomic resource at `canonical_path`, claiming
    /// `<canonical>.tmp` via `OpenOptions::create_new`. See the crate
    /// `README.md` "Chunked atomic claim" for the `factory`, claim, and
    /// stale-temp semantics.
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
        F: Fn(&Path, OpenIntent) -> StorageResult<R> + Send + Sync + 'static,
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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(StorageError::TmpClaimed(tmp_path));
            }
            Err(e) => return Err(StorageError::Io(e)),
        }
        let inner = factory(&tmp_path, OpenIntent::Fresh)?;
        Ok(Self {
            canonical_path,
            inner: Mutex::new(Arc::new(inner)),
            tmp_path: Mutex::new(Some(tmp_path)),
            factory: Some(Box::new(factory)),
        })
    }

    /// Wrap an already-opened inner with no atomicity (pass-through).
    /// Used for memory-backed inners that have no filesystem to
    /// protect, or for re-opens of files that are already committed
    /// on disk.
    pub fn passthrough(inner: R, canonical_path: PathBuf) -> Self {
        Self {
            canonical_path,
            inner: Mutex::new(Arc::new(inner)),
            tmp_path: Mutex::new(None),
            factory: None,
        }
    }
}

impl<R: ResourceExt> ResourceExt for AtomicChunked<R> {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.inner_clone().commit(final_len)?;

        let Some(tmp) = self.tmp_path.lock_sync().take() else {
            return Ok(());
        };
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
            let new_inner = Arc::new(factory(&self.canonical_path, OpenIntent::Reopen)?);
            *self.inner.lock_sync() = new_inner;
        }
        Ok(())
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.inner_clone().contains_range(range)
    }

    fn fail(&self, reason: String) {
        self.inner_clone().fail(reason);
        if let Some(tmp) = self.tmp_path.lock_sync().take() {
            let _ = fs::remove_file(&tmp);
        }
    }

    fn len(&self) -> Option<u64> {
        self.inner_clone().len()
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.inner_clone().next_gap(from, limit)
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.canonical_path)
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.inner_clone().reactivate()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner_clone().read_at(offset, buf)
    }

    fn status(&self) -> ResourceStatus {
        self.inner_clone().status()
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.inner_clone().wait_range(range)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner_clone().write_at(offset, data)
    }
}

impl<R: ResourceExt> Drop for AtomicChunked<R> {
    /// Clean up the orphaned temp file when a writer is dropped
    /// without a successful commit. Best-effort: a `kill -9` skips
    /// `Drop` entirely, in which case the next `AtomicChunked::open`
    /// over the same canonical path wipes the stale temp.
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp_path.lock_sync().take() {
            let _ = fs::remove_file(&tmp);
        }
    }
}
