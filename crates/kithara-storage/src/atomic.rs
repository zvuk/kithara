#![forbid(unsafe_code)]

//! Crash-safe whole-file write decorator.
//!
//! [`Atomic<R>`] wraps any [`ResourceExt`] and makes [`write_all()`](ResourceExt::write_all)
//! crash-safe via the write-temp → fsync → rename pattern.
//!
//! For file-backed resources, `write_all()` writes to a `.tmp` sibling, fsyncs,
//! then atomically renames over the target path. For in-memory resources,
//! `write_all()` delegates directly (no filesystem to protect).

use std::{ops::Range, path::Path};

use crate::{ResourceExt, ResourceStatus, StorageResult, WaitOutcome};

/// Decorator for crash-safe whole-file writes.
///
/// For file-backed resources: write-rename pattern ensures that the target file
/// is either the old version or the new version — never a partial write.
///
/// For in-memory resources: direct delegation (crash-safety is not applicable).
pub struct Atomic<R: ResourceExt> {
    inner: R,
}

impl<R: ResourceExt + Clone> Clone for Atomic<R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R: ResourceExt + std::fmt::Debug> std::fmt::Debug for Atomic<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atomic")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<R: ResourceExt> Atomic<R> {
    /// Wrap a resource for crash-safe writes.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: ResourceExt> ResourceExt for Atomic<R> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.inner.wait_range(range)
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.inner.commit(final_len)
    }

    fn fail(&self, reason: String) {
        self.inner.fail(reason);
    }

    fn path(&self) -> Option<&Path> {
        self.inner.path()
    }

    fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    fn status(&self) -> ResourceStatus {
        self.inner.status()
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.inner.reactivate()
    }

    fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        match self.inner.path() {
            Some(path) => {
                let path = path.to_path_buf();
                let tmp = path.with_extension("tmp");

                // 1. Ensure parent directory exists.
                if let Some(parent) = tmp.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }

                // 2. Write to temp file.
                std::fs::write(&tmp, data)
                    .map_err(|e| crate::StorageError::Failed(format!("atomic write tmp: {e}")))?;

                // 3. fsync temp file.
                let f = std::fs::File::open(&tmp)
                    .map_err(|e| crate::StorageError::Failed(format!("atomic fsync open: {e}")))?;
                f.sync_all()
                    .map_err(|e| crate::StorageError::Failed(format!("atomic fsync: {e}")))?;

                // 4. Atomic rename.
                std::fs::rename(&tmp, &path)
                    .map_err(|e| crate::StorageError::Failed(format!("atomic rename: {e}")))?;

                // 5. fsync parent directory (best-effort).
                if let Some(parent) = path.parent()
                    && let Ok(d) = std::fs::File::open(parent)
                {
                    let _ = d.sync_all();
                }

                // 6. commit() re-opens the file by path — sees new data after rename.
                self.inner.commit(Some(data.len() as u64))
            }
            None => {
                // In-memory: delegate directly.
                self.inner.write_all(data)
            }
        }
    }
}

/// Crash-safe mmap-backed resource.
pub type AtomicMmap = Atomic<crate::MmapResource>;

/// Crash-safe in-memory resource (delegation only).
pub type AtomicMem = Atomic<crate::MemResource>;

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{MemOptions, MemResource, MmapOptions, MmapResource, OpenMode, Resource};

    fn create_mmap_resource(dir: &TempDir, name: &str) -> MmapResource {
        let path = dir.path().join(name);
        Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap()
    }

    fn create_mem_resource() -> MemResource {
        Resource::open(CancellationToken::new(), MemOptions { initial_data: None }).unwrap()
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mmap_write_all_read_into_roundtrip() {
        let dir = TempDir::new().unwrap();
        let res = create_mmap_resource(&dir, "test.bin");
        let atomic = Atomic::new(res);

        let data = b"hello atomic world";
        atomic.write_all(data).unwrap();

        let mut buf = Vec::new();
        let n = atomic.read_into(&mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&buf, data);
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mmap_tmp_file_cleaned_up() {
        let dir = TempDir::new().unwrap();
        let res = create_mmap_resource(&dir, "index.bin");
        let atomic = Atomic::new(res);

        atomic.write_all(b"data").unwrap();

        let tmp_path = dir.path().join("index.tmp");
        assert!(!tmp_path.exists(), "tmp file should not remain after write");
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mem_write_all_read_into_roundtrip() {
        let res = create_mem_resource();
        let atomic = Atomic::new(res);

        let data = b"in-memory data";
        atomic.write_all(data).unwrap();

        let mut buf = Vec::new();
        let n = atomic.read_into(&mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&buf, data);
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mmap_read_into_empty_returns_zero() {
        let dir = TempDir::new().unwrap();
        let res = create_mmap_resource(&dir, "empty.bin");
        let atomic = Atomic::new(res);

        let mut buf = Vec::new();
        let n = atomic.read_into(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mmap_overwrite_atomically() {
        let dir = TempDir::new().unwrap();
        let res = create_mmap_resource(&dir, "overwrite.bin");
        let atomic = Atomic::new(res);

        atomic.write_all(b"first version").unwrap();
        atomic.write_all(b"second version - longer data").unwrap();

        let mut buf = Vec::new();
        let n = atomic.read_into(&mut buf).unwrap();
        assert_eq!(n, b"second version - longer data".len());
        assert_eq!(&buf, b"second version - longer data");
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mmap_path_returns_inner_path() {
        let dir = TempDir::new().unwrap();
        let res = create_mmap_resource(&dir, "path_test.bin");
        let expected = dir.path().join("path_test.bin");
        let atomic = Atomic::new(res);

        assert_eq!(atomic.path(), Some(expected.as_path()));
    }

    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[test]
    fn mem_path_returns_none() {
        let res = create_mem_resource();
        let atomic = Atomic::new(res);

        assert!(atomic.path().is_none());
    }
}
