#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write};
use std::{ops::Range, path::Path};

#[cfg(not(target_arch = "wasm32"))]
use tempfile::NamedTempFile;

use crate::{
    ResourceRead, ResourceStatus, ResourceWriter, StorageResult, WaitOutcome,
    backend::traits::DriverIo,
};

/// Decorator for crash-safe whole-file writes over a single-owner
/// [`ResourceWriter`].
///
/// For file-backed resources: write-rename pattern ensures that the target file
/// is either the old version or the new version — never a partial write. The
/// file is rewritten in place across flushes (`OpenMode::ReadWrite` index
/// files), so the wrapped writer commits in place rather than being consumed.
///
/// For in-memory resources: direct delegation (crash-safety is not applicable).
pub struct Atomic<D: DriverIo> {
    inner: ResourceWriter<D>,
}

impl<D: DriverIo> Atomic<D> {
    /// Wrap a writer for crash-safe writes.
    #[must_use]
    pub fn new(inner: ResourceWriter<D>) -> Self {
        Self { inner }
    }

    /// Whether the given range is fully covered by available data.
    #[must_use]
    pub fn contains_range(&self, range: Range<u64>) -> bool {
        self.inner.contains_range(range)
    }

    /// Returns `true` if the resource has been committed with zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Committed length, if known.
    #[must_use]
    pub fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    /// Backing file path, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.inner.path()
    }

    /// Read data at the given offset into `buf`.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner.read_at(offset, buf)
    }

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    pub fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        self.inner.read_into(buf)
    }

    /// Current runtime status.
    #[must_use]
    pub fn status(&self) -> ResourceStatus {
        self.inner.status()
    }

    /// Wait until the given byte range is available.
    ///
    /// # Errors
    /// Returns error if the range is invalid, the resource is cancelled, or the
    /// resource has failed.
    pub fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.inner.wait_range(range)
    }

    /// Write the whole payload atomically (best-effort durability: atomic rename
    /// only). Rewrites the file in place.
    ///
    /// # Errors
    /// Propagates filesystem errors and the inner commit error.
    pub fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        self.write_all_inner(data, false)
    }

    /// Write the whole payload atomically AND durably: like [`Atomic::write_all`]
    /// but also `sync_data`s the temp file before the atomic rename. Returns
    /// only after the bytes are physically on disk.
    ///
    /// For memory-backed inners (no filesystem path) durability is meaningless;
    /// the call falls back to the same passthrough as `write_all`.
    ///
    /// # Errors
    /// Propagates filesystem errors from temp creation, write, `sync_data`,
    /// rename, or the inner's post-rename commit.
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

            return self.inner.commit_in_place(Some(data.len() as u64));
        }

        let _ = durable;
        self.inner.reactivate_in_place()?;
        self.inner.write_at(0, data)?;
        self.inner.commit_in_place(Some(data.len() as u64))
    }
}
