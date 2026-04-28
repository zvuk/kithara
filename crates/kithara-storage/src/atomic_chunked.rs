#![forbid(unsafe_code)]

//! Crash-safe **chunked** write decorator.
//!
//! [`AtomicChunked<R>`] wraps any [`ResourceExt`] and makes the
//! `write_at … write_at … commit` lifecycle atomic at the file level
//! via the write-temp → `sync_data` → rename pattern.
//!
//! Sibling to [`Atomic<R>`](crate::Atomic):
//!
//! - [`Atomic<R>`] makes one-shot **whole-file** rewrites
//!   ([`ResourceExt::write_all`]) atomic. Used by index files.
//! - [`AtomicChunked<R>`] makes incremental **chunked**
//!   ([`ResourceExt::write_at`] + [`ResourceExt::commit`]) atomic.
//!   Used by segment files where data arrives in pieces.
//!
//! Both decorators are generic over the inner resource type. For
//! file-backed inners (`path() == Some(_)`) the tmp+rename pattern
//! kicks in; for memory-backed inners it is a no-op pass-through (no
//! filesystem to protect).
//!
//! ## Lifecycle
//!
//! - Open the inner on the **tmp path** (sibling `.tmp` of canonical)
//!   via the [`AtomicChunked::open`] factory.
//! - `write_at` chunks: routed straight to inner.
//! - `commit(final_len)`:
//!     1. `inner.commit(final_len)` — finalizes inner state.
//!     2. `sync_data` on the tmp file — durably flushes payload pages.
//!     3. `fs::rename(tmp → canonical)` — atomic at the directory
//!        entry level (POSIX guarantee).
//! - `fail` / `Drop` without a successful commit: tmp file is removed
//!   so the canonical path never inherits partial bytes.
//!
//! ## Reads during writes
//!
//! Readers go through the decorator (`read_at`, `wait_range`) and hit
//! the inner mmap on the tmp inode. After `commit`, the canonical
//! entry now points at the same inode (`rename` did not change the
//! inode). The inner mmap remains valid post-rename.

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
fn make_tmp_path(canonical: &Path) -> Option<PathBuf> {
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
    canonical_path: PathBuf,
    /// `Some(<path>.tmp)` while writes are in flight; cleared on
    /// successful `commit`. `Drop` / `fail` use a still-set value to
    /// remove the orphaned temp file.
    tmp_path: Mutex<Option<PathBuf>>,
    /// Factory to reopen the inner on the canonical path post-rename.
    /// `None` when the wrapper was constructed in passthrough mode
    /// (no atomic rename to perform, no reopen needed).
    factory: Option<FactoryFn<R>>,
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
    /// Open a fresh chunked-atomic resource at `canonical_path`.
    /// The provided `factory` opens the inner at a given filesystem
    /// path; it is called once with the temp path during this
    /// constructor and once more with the canonical path after the
    /// atomic rename in [`ResourceExt::commit`].
    ///
    /// Any stale temp left from a prior crashed run is wiped first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Failed`] if the canonical path has no
    /// parent / non-utf8 file name (cannot derive a temp companion),
    /// or if the supplied factory fails to open the inner.
    pub fn open<F>(canonical_path: PathBuf, factory: F) -> StorageResult<Self>
    where
        F: Fn(&Path, OpenIntent) -> StorageResult<R> + Send + Sync + 'static,
    {
        let tmp_path = make_tmp_path(&canonical_path).ok_or_else(|| {
            StorageError::Failed(format!(
                "AtomicChunked: cannot derive tmp path from {canonical_path:?}"
            ))
        })?;
        // Wipe a stale temp file from a previous crashed run before
        // we mmap on top of it.
        let _ = fs::remove_file(&tmp_path);
        let inner = factory(&tmp_path, OpenIntent::Fresh)?;
        Ok(Self {
            inner: Mutex::new(Arc::new(inner)),
            canonical_path,
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
            inner: Mutex::new(Arc::new(inner)),
            canonical_path,
            tmp_path: Mutex::new(None),
            factory: None,
        }
    }

    /// Clone the inner Arc — caller can call any `&self` method on
    /// the returned handle without holding the outer mutex.
    fn inner_clone(&self) -> Arc<R> {
        Arc::clone(&self.inner.lock_sync())
    }

    /// Path the resource will land at on a successful commit.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

impl<R: ResourceExt> ResourceExt for AtomicChunked<R> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner_clone().read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner_clone().write_at(offset, data)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        // Don't hold the outer mutex across this potentially
        // blocking call — clone the inner Arc and release the lock.
        // Inner has its own Condvar for blocking-wake; the outer
        // mutex is purely for swapping the inner on commit.
        self.inner_clone().wait_range(range)
    }

    fn len(&self) -> Option<u64> {
        self.inner_clone().len()
    }

    fn status(&self) -> ResourceStatus {
        self.inner_clone().status()
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.inner_clone().reactivate()
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.inner_clone().contains_range(range)
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.inner_clone().next_gap(from, limit)
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        // Step 1: finalize the inner on the tmp file. After this
        // call the inner is Committed and its mmap is RO on the tmp
        // inode.
        self.inner_clone().commit(final_len)?;

        // Step 2: pull the tmp path. Take() so a subsequent Drop /
        // fail does not try to remove a file we are about to rename.
        let tmp = self.tmp_path.lock_sync().take();
        if let Some(tmp) = tmp {
            // Step 3: durably flush payload + metadata to disk.
            // `sync_data` requires write access on Linux (per stdlib
            // docs); use `write(true)` for portability.
            let f = OpenOptions::new().write(true).open(&tmp).map_err(|e| {
                StorageError::Failed(format!("AtomicChunked commit: open tmp {tmp:?}: {e}"))
            })?;
            f.sync_data().map_err(|e| {
                StorageError::Failed(format!("AtomicChunked commit: sync_data {tmp:?}: {e}"))
            })?;
            drop(f);
            // Step 4: atomic rename. POSIX guarantees this is atomic
            // at the directory-entry level: any reader doing
            // open(canonical) sees either the old (no) file or the
            // newly committed bytes.
            fs::rename(&tmp, &self.canonical_path).map_err(|e| {
                StorageError::Failed(format!(
                    "AtomicChunked commit: rename {tmp:?} -> {:?}: {e}",
                    self.canonical_path
                ))
            })?;

            // Step 5: reopen inner on the canonical path. The previous
            // inner's mmap was bound to the renamed inode (still
            // valid for reads), but its `path()` was the now-gone tmp
            // path. Reopening on canonical eliminates that staleness:
            // every internal fs op of the inner (truncate, reopen,
            // metadata) targets a file that actually exists.
            if let Some(factory) = self.factory.as_ref() {
                let new_inner = Arc::new(factory(&self.canonical_path, OpenIntent::Reopen)?);
                *self.inner.lock_sync() = new_inner;
            }
        }
        Ok(())
    }

    fn fail(&self, reason: String) {
        self.inner_clone().fail(reason);
        if let Some(tmp) = self.tmp_path.lock_sync().take() {
            let _ = fs::remove_file(&tmp);
        }
    }

    fn path(&self) -> Option<&Path> {
        // User-facing canonical path. Slow-path lookups
        // (`fs::metadata`, external scanners) see this name —
        // overridden because `inner.path()` would still return the
        // tmp path during the pre-commit window.
        Some(&self.canonical_path)
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

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use kithara_platform::time::Duration;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{MmapOptions, MmapResource, OpenMode, Resource};

    fn open_chunked(dir: &TempDir, name: &str) -> (AtomicChunked<MmapResource>, PathBuf, PathBuf) {
        let canonical = dir.path().join(name);
        let cancel = CancellationToken::new();
        let res = AtomicChunked::<MmapResource>::open(canonical.clone(), move |target, intent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open(
                cancel.clone(),
                MmapOptions {
                    path: target.to_path_buf(),
                    initial_len: None,
                    mode,
                },
            )
        })
        .unwrap();
        let tmp = make_tmp_path(&canonical).unwrap();
        (res, canonical, tmp)
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn canonical_invisible_until_commit() {
        let dir = TempDir::new().unwrap();
        let (res, canonical, tmp) = open_chunked(&dir, "seg.bin");

        res.write_at(0, b"chunk-1-").unwrap();
        res.write_at(8, b"chunk-2!").unwrap();

        assert!(
            !canonical.exists(),
            "canonical must not exist before commit"
        );
        assert!(tmp.exists(), "tmp file must hold in-flight bytes");

        res.commit(Some(16)).unwrap();
        assert!(canonical.exists(), "canonical materialised on commit");
        assert!(!tmp.exists(), "tmp consumed by atomic rename");

        let bytes = fs::read(&canonical).unwrap();
        assert_eq!(&bytes, b"chunk-1-chunk-2!");
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn drop_without_commit_cleans_tmp() {
        let dir = TempDir::new().unwrap();
        let (res, canonical, tmp) = open_chunked(&dir, "abandoned.bin");

        res.write_at(0, b"will-not-commit").unwrap();
        assert!(tmp.exists());
        drop(res);

        assert!(!tmp.exists(), "Drop must remove the orphaned tmp");
        assert!(!canonical.exists(), "canonical must never appear");
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn fail_cleans_tmp() {
        let dir = TempDir::new().unwrap();
        let (res, canonical, tmp) = open_chunked(&dir, "failed.bin");

        res.write_at(0, b"oops").unwrap();
        res.fail("test".into());

        assert!(!tmp.exists(), "fail() must remove the tmp");
        assert!(!canonical.exists());
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn open_wipes_stale_tmp_from_previous_run() {
        let dir = TempDir::new().unwrap();
        let canonical = dir.path().join("survivor.bin");
        let stale_tmp = make_tmp_path(&canonical).unwrap();
        // Simulate a crashed previous writer that left orphan bytes.
        fs::write(&stale_tmp, b"stale-from-previous-process").unwrap();

        let cancel = CancellationToken::new();
        let res = AtomicChunked::<MmapResource>::open(canonical.clone(), move |target, intent| {
            let mode = match intent {
                OpenIntent::Fresh => OpenMode::ReadWrite,
                OpenIntent::Reopen => OpenMode::ReadOnly,
            };
            Resource::open(
                cancel.clone(),
                MmapOptions {
                    path: target.to_path_buf(),
                    initial_len: None,
                    mode,
                },
            )
        })
        .unwrap();
        res.write_at(0, b"fresh").unwrap();
        res.commit(Some(5)).unwrap();
        let bytes = fs::read(&canonical).unwrap();
        assert_eq!(&bytes, b"fresh");
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn read_after_commit_returns_payload_via_decorator() {
        // Smoke: write chunks, commit, then read back THROUGH the
        // decorator (not direct fs::read). Pins that the inner mmap
        // remains usable after the rename — reads must surface the
        // committed payload exactly.
        let dir = TempDir::new().unwrap();
        let (res, _, _) = open_chunked(&dir, "post-commit-read.bin");
        res.write_at(0, b"chunk-1-").unwrap();
        res.write_at(8, b"chunk-2!").unwrap();
        res.commit(Some(16)).unwrap();

        let mut buf = [0u8; 16];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 16, "post-commit read must return all bytes");
        assert_eq!(&buf, b"chunk-1-chunk-2!");

        // Read of the tail (last byte) — exact same condition Apple's
        // probe hits in the failing integration test.
        let mut tail = [0u8; 1];
        let n = res.read_at(15, &mut tail).unwrap();
        assert_eq!(n, 1);
        assert_eq!(tail[0], b'!');
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn read_during_writes_observes_inner_state() {
        let dir = TempDir::new().unwrap();
        let (res, _, _) = open_chunked(&dir, "live.bin");
        res.write_at(0, b"live-bytes").unwrap();
        let mut buf = [0u8; 10];
        let n = res.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, b"live-bytes");
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn passthrough_for_memory_inner_has_no_tmp() {
        // For mem-backed inners (no filesystem), the decorator
        // delegates straight through and never creates a tmp file.
        let mem = crate::MemResource::new(CancellationToken::new());
        let res = AtomicChunked::passthrough(mem, PathBuf::from("virtual"));
        res.write_at(0, b"in-mem").unwrap();
        res.commit(Some(6)).unwrap();
        let mut buf = [0u8; 6];
        res.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"in-mem");
        // path() reports canonical; nothing on disk.
        assert_eq!(res.path(), Some(Path::new("virtual")));
    }
}
