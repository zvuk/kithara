#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write};
use std::{ops::Range, path::Path};

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

            let parent = path.parent().ok_or_else(|| {
                crate::StorageError::Failed("atomic write: no parent dir".to_string())
            })?;
            let _ = fs::create_dir_all(parent);

            let mut tmp = NamedTempFile::new_in(parent)
                .map_err(|e| crate::StorageError::Failed(format!("atomic write tmpfile: {e}")))?;

            Write::write_all(&mut tmp, data)
                .map_err(|e| crate::StorageError::Failed(format!("atomic write: {e}")))?;

            if durable {
                tmp.as_file()
                    .sync_data()
                    .map_err(|e| crate::StorageError::Failed(format!("atomic sync_data: {e}")))?;
            }

            tmp.persist(&path)
                .map_err(|e| crate::StorageError::Failed(format!("atomic rename: {e}")))?;

            return self.inner.commit(Some(data.len() as u64));
        }

        let _ = durable;
        self.inner.reactivate()?;
        self.inner.write_all(data)
    }
}
