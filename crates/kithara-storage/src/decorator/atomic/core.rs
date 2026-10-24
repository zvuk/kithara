#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write};
use std::{ops::Range, path::Path};

use kithara_platform::{CancelToken, sync::RwLock};
#[cfg(not(target_arch = "wasm32"))]
use tempfile::NamedTempFile;

use crate::{
    ResourceRead, ResourceStatus, ResourceWriter, StorageResult, WaitOutcome,
    backend::traits::{Driver, DriverIo},
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
    handover: RwLock<()>,
}

impl<D: Driver> Atomic<D> {
    /// Open a resource whose readers remain inside this decorator's handover.
    ///
    /// # Errors
    /// Returns an error if the backing resource cannot be opened.
    pub fn open(cancel: CancelToken, opts: D::Options) -> StorageResult<Self> {
        Ok(Self {
            inner: ResourceWriter::open(cancel, opts)?,
            handover: RwLock::default(),
        })
    }
}

impl<D: DriverIo> Atomic<D> {
    /// Returns `true` if the resource has been committed with zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Write the whole payload atomically (best-effort durability: atomic rename
    /// only). Rewrites the file in place.
    ///
    /// # Errors
    /// Propagates filesystem errors and the inner commit error.
    ///
    /// The inner resource still maps the canonical path, and Windows refuses to replace a mapped
    /// file, so this branch commits and reopens it instead of renaming over it.
    pub fn write_all(&self, data: &[u8]) -> StorageResult<()> {
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

            let _handover = self.handover.write();
            self.inner.release_backing_in_place(None)?;

            tmp.persist(&path)
                .map_err(|e| crate::StorageError::Failed(format!("atomic rename: {e}")))?;

            return self.inner.commit_in_place(Some(data.len() as u64));
        }

        let _handover = self.handover.write();
        self.inner.reactivate_in_place()?;
        self.inner.write_at(0, data)?;
        self.inner.commit_in_place(Some(data.len() as u64))
    }

    #[cfg(test)]
    pub(super) fn read_settled<R>(&self, read: impl FnOnce(&ResourceWriter<D>) -> R) -> R {
        let _handover = self.handover.read();
        read(&self.inner)
    }

    delegate::delegate! {
        to self.inner {
            /// Whether the given range is fully covered by available data.
            #[must_use]
            #[expr({ let _handover = self.handover.read(); $ })]
            pub fn contains_range(&self, range: Range<u64>) -> bool;
            /// Committed length, if known.
            #[must_use]
            #[expr({ let _handover = self.handover.read(); $ })]
            pub fn len(&self) -> Option<u64>;
            /// Backing file path, if any.
            #[must_use]
            pub fn path(&self) -> Option<&Path>;
            /// Read data at the given offset into `buf`.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the read fails.
            #[expr({ let _handover = self.handover.read(); $ })]
            pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            /// Read the entire resource into a caller buffer; returns bytes read.
            ///
            /// # Errors
            /// Returns error if the resource is cancelled, failed, or the read fails.
            #[expr({ let _handover = self.handover.read(); $ })]
            pub fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize>;
            /// Runtime status of the inner resource.
            #[must_use]
            #[expr({ let _handover = self.handover.read(); $ })]
            pub fn status(&self) -> ResourceStatus;
            /// Wait until the given byte range is available.
            ///
            /// # Errors
            /// Returns error if the range is invalid, the resource is cancelled, or the
            /// resource has failed.
            pub fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            /// Wait until the range is available, interrupting only this wait
            /// when `cancel` fires.
            ///
            /// # Errors
            /// Returns error if the range is invalid, either cancellation token
            /// fires, or the resource has failed.
            pub fn wait_range_with_cancel(
                &self,
                range: Range<u64>,
                cancel: &CancelToken,
            ) -> StorageResult<WaitOutcome>;
        }
    }
}
