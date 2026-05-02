#![forbid(unsafe_code)]

//! Crash-safe whole-file write decorator.
//!
//! [`Atomic<R>`] wraps any [`ResourceExt`] and makes [`write_all()`](ResourceExt::write_all)
//! crash-safe via the write-temp → rename pattern.
//!
//! For file-backed resources, `write_all()` writes to a uniquely-named temp file
//! (via the `tempfile` crate), then atomically renames over the target path.
//! For in-memory resources, `write_all()` delegates directly (no filesystem to protect).

#[cfg(not(target_arch = "wasm32"))]
use std::fs;
use std::{io::Write, ops::Range, path::Path};

#[cfg(not(target_arch = "wasm32"))]
use tempfile::NamedTempFile;

use crate::{ResourceExt, ResourceStatus, StorageResult, WaitOutcome};

/// Decorator for crash-safe whole-file writes.
///
/// For file-backed resources: write-rename pattern ensures that the target file
/// is either the old version or the new version — never a partial write.
///
/// For in-memory resources: direct delegation (crash-safety is not applicable).
#[derive(Clone, Debug)]
pub struct Atomic<R: ResourceExt> {
    inner: R,
}

impl<R: ResourceExt> Atomic<R> {
    /// Wrap a resource for crash-safe writes.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: ResourceExt> ResourceExt for Atomic<R> {
    fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        self.write_all_inner(data, false)
    }

    delegate::delegate! {
        to self.inner {
            fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;
            fn fail(&self, reason: String);
            fn len(&self) -> Option<u64>;
            fn path(&self) -> Option<&Path>;
            fn reactivate(&self) -> StorageResult<()>;
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn status(&self) -> ResourceStatus;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }
}

impl<R: ResourceExt> Atomic<R> {
    /// Write the whole payload atomically AND durably: like
    /// [`ResourceExt::write_all`] but also `sync_data`s the temp file
    /// before the atomic rename. Returns only after the bytes are
    /// physically on disk.
    ///
    /// Use this on explicit checkpoint paths where the caller wants
    /// post-return durability. Per-mutation flushes should keep using
    /// the cheaper [`ResourceExt::write_all`] (best-effort, atomic
    /// rename only).
    ///
    /// For memory-backed inners (no filesystem path) durability is
    /// meaningless; the call falls back to the same passthrough as
    /// `write_all`.
    ///
    /// # Errors
    ///
    /// Propagates filesystem errors from temp creation, write,
    /// `sync_data`, rename, or the inner's post-rename `commit`.
    pub fn write_all_durable(&self, data: &[u8]) -> StorageResult<()> {
        self.write_all_inner(data, true)
    }

    fn write_all_inner(&self, data: &[u8], durable: bool) -> StorageResult<()> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = self.inner.path() {
            let path = path.to_path_buf();

            // Parent directory for temp file (same filesystem = rename is atomic).
            let parent = path.parent().ok_or_else(|| {
                crate::StorageError::Failed("atomic write: no parent dir".to_string())
            })?;
            let _ = fs::create_dir_all(parent);

            // 1. Create unique temp file via `tempfile` crate.
            let mut tmp = NamedTempFile::new_in(parent)
                .map_err(|e| crate::StorageError::Failed(format!("atomic write tmpfile: {e}")))?;

            // 2. Write data to temp file.
            Write::write_all(&mut tmp, data)
                .map_err(|e| crate::StorageError::Failed(format!("atomic write: {e}")))?;

            // 3. Optional durability fence: `sync_data` forces the
            //    payload pages to physical disk before rename.
            //    POSIX guarantees rename atomicity, but not data
            //    durability of the renamed file's contents — without
            //    this fence a power loss between rename and the
            //    kernel's lazy flush leaves a torn file under `path`.
            if durable {
                tmp.as_file()
                    .sync_data()
                    .map_err(|e| crate::StorageError::Failed(format!("atomic sync_data: {e}")))?;
            }

            // 4. Atomic rename (POSIX guarantees atomicity).
            //    `persist()` does `rename(tmp, target)` and disarms the
            //    auto-delete on drop.
            tmp.persist(&path)
                .map_err(|e| crate::StorageError::Failed(format!("atomic rename: {e}")))?;

            // 5. Re-open by path — sees new data after rename.
            //    commit() drops the old mmap (now stale) and opens the
            //    renamed file as read-only.
            return self.inner.commit(Some(data.len() as u64));
        }

        // In-memory or wasm32: reactivate committed resources before overwrite.
        // Durability is meaningless without a filesystem; both paths fall
        // through to the same passthrough.
        let _ = durable;
        self.inner.reactivate()?;
        self.inner.write_all(data)
    }
}

/// Crash-safe mmap-backed resource.
#[cfg(not(target_arch = "wasm32"))]
pub type AtomicMmap = Atomic<crate::MmapResource>;
