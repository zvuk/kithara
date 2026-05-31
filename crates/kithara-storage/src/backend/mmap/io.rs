#![forbid(unsafe_code)]

use std::{
    fs::{self, OpenOptions},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use mmap_io::MemoryMappedFile;

use crate::{
    StorageError, StorageResult,
    backend::{
        mmap::driver::{Consts, MmapDriver, MmapState},
        traits::DriverIo,
    },
    resource::OpenMode,
};

impl DriverIo for MmapDriver {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let mut mmap_guard = self.mmap.lock_sync();

        // A re-download (`reactivate`) wrote the new generation to a temp file so
        // it never aliased the still-published committed snapshot. Detect that
        // here: the new generation is swapped into place with an atomic rename,
        // which keeps the old inode alive for in-flight readers holding the old
        // RO mmap. An initial download writes `self.path` in place (no temp).
        let rewrite_temp: Option<PathBuf> = match &*mmap_guard {
            MmapState::Active(m) if m.path() != self.path => Some(m.path().to_path_buf()),
            _ => None,
        };

        if let Some(len) = final_len {
            if len > 0 {
                let needs_truncate = matches!(
                    &*mmap_guard,
                    MmapState::Active(mmap) if len < mmap.len()
                );

                // Flush the temp generation's dirty pages before dropping the
                // map and renaming, so the republished RO mmap sees them.
                if rewrite_temp.is_some()
                    && let MmapState::Active(m) = &*mmap_guard
                {
                    m.flush()?;
                }

                *mmap_guard = MmapState::Empty;

                let source = rewrite_temp.as_deref().unwrap_or(&self.path);

                if needs_truncate {
                    let file_len = fs::metadata(source).map_or(0, |m| m.len());
                    if file_len > len {
                        let f = OpenOptions::new()
                            .write(true)
                            .open(source)
                            .map_err(|e| StorageError::Failed(format!("truncate open: {e}")))?;
                        f.set_len(len)
                            .map_err(|e| StorageError::Failed(format!("truncate: {e}")))?;
                    }
                }

                if let Some(temp) = rewrite_temp.as_deref() {
                    fs::rename(temp, &self.path)
                        .map_err(|e| StorageError::Failed(format!("rewrite rename: {e}")))?;
                }

                let arc = Arc::new(MemoryMappedFile::open_ro(&self.path)?);
                *mmap_guard = MmapState::Committed(Arc::clone(&arc));
                self.committed.store(Some(arc));
            } else {
                self.committed.store(None);
                *mmap_guard = MmapState::Empty;
                if let Some(temp) = rewrite_temp.as_deref() {
                    let _ = fs::remove_file(temp);
                }
                let _ = fs::write(&self.path, b"");
            }
        } else {
            let is_active = matches!(*mmap_guard, MmapState::Active(_));
            if is_active {
                if rewrite_temp.is_some()
                    && let MmapState::Active(m) = &*mmap_guard
                {
                    m.flush()?;
                }
                *mmap_guard = MmapState::Empty;
                if let Some(temp) = rewrite_temp.as_deref() {
                    fs::rename(temp, &self.path)
                        .map_err(|e| StorageError::Failed(format!("rewrite rename: {e}")))?;
                }
                if self.path.exists() && fs::metadata(&self.path).is_ok_and(|m| m.len() > 0) {
                    let arc = Arc::new(MemoryMappedFile::open_ro(&self.path)?);
                    *mmap_guard = MmapState::Committed(Arc::clone(&arc));
                    self.committed.store(Some(arc));
                }
            }
        }

        drop(mmap_guard);
        Ok(())
    }

    fn committed_len(&self) -> Option<u64> {
        self.committed.load().as_ref().map(|m| m.len())
    }

    fn notify_write(&self, range: &Range<u64>) {
        self.ready_ranges.push(range.clone());
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn reactivate(&self) -> StorageResult<()> {
        let mut mmap_guard = self.mmap.lock_sync();

        match &*mmap_guard {
            // Already active (initial download in flight) — nothing to do.
            MmapState::Active(_) => {}
            // Re-download. Keep the committed snapshot PUBLISHED so in-flight
            // readers keep serving the immutable prior generation zero-copy via
            // the old RO mmap. Write the new generation to a fresh temp file;
            // `commit` atomically renames it into place. The committed file
            // mapped by the snapshot is never overwritten in place, so a
            // concurrent read can never tear across the generation boundary.
            MmapState::Committed(_) | MmapState::Empty => {
                let temp = self.rewrite_temp_path();
                // Drop any stale temp left by a previously-cancelled rewrite.
                let _ = fs::remove_file(&temp);
                let rw = MemoryMappedFile::create_rw(&temp, Consts::DEFAULT_INITIAL_SIZE)?;
                *mmap_guard = MmapState::Active(rw);
            }
        }

        drop(mmap_guard);
        Ok(())
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        {
            let mmap_guard = self.mmap.lock_sync();
            if let Some(mmap) = mmap_guard.as_readable() {
                mmap.read_into(offset, buf)?;
            }
        }
        Ok(buf.len())
    }

    fn read_committed(&self, offset: u64, buf: &mut [u8]) -> StorageResult<Option<usize>> {
        let snapshot = self.committed.load();
        let Some(mmap) = snapshot.as_ref() else {
            return Ok(None);
        };

        let len = mmap.len();
        if offset >= len {
            return Ok(Some(0));
        }

        let buf_len = u64::try_from(buf.len()).map_err(|err| {
            StorageError::Failed(format!(
                "mmap read_committed: buf len {} does not fit u64: {err}",
                buf.len()
            ))
        })?;
        let to_read = usize::try_from((len - offset).min(buf_len)).map_err(|err| {
            StorageError::Failed(format!(
                "mmap read_committed: read count does not fit usize: {err}"
            ))
        })?;
        mmap.read_into(offset, &mut buf[..to_read])?;
        Ok(Some(to_read))
    }

    fn storage_len(&self) -> u64 {
        let mmap_guard = self.mmap.lock_sync();
        mmap_guard.len()
    }

    fn try_fast_check(&self, range: &Range<u64>) -> bool {
        std::iter::from_fn(|| self.ready_ranges.pop())
            .any(|ready| ready.start <= range.start && ready.end >= range.end)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        let end = offset + data.len() as u64;
        let mut mmap_guard = self.mmap.lock_sync();

        if committed {
            match (&*mmap_guard, self.mode) {
                (MmapState::Committed(_), OpenMode::ReadWrite) => {
                    self.committed.store(None);
                    *mmap_guard = MmapState::Empty;
                    let rw = MemoryMappedFile::open_rw(&self.path)?;
                    *mmap_guard = MmapState::Active(rw);
                }
                (MmapState::Active(_), _)
                | (MmapState::Empty, OpenMode::Auto | OpenMode::ReadWrite) => {}
                _ => {
                    return Err(StorageError::Failed(
                        "cannot write to committed resource".to_string(),
                    ));
                }
            }
        }

        let mmap = match &*mmap_guard {
            MmapState::Active(m) => {
                if end > m.len() {
                    let new_size = end.max(m.len() * Consts::MMAP_GROWTH_FACTOR);
                    m.resize(new_size)?;
                }
                m
            }
            MmapState::Committed(_) => {
                return Err(StorageError::Failed(
                    "cannot write to committed resource".to_string(),
                ));
            }
            MmapState::Empty => {
                let size = end.max(Consts::DEFAULT_INITIAL_SIZE);
                let m = MemoryMappedFile::create_rw(&self.path, size)?;
                *mmap_guard = MmapState::Active(m);
                match &*mmap_guard {
                    MmapState::Active(m) => m,
                    _ => {
                        return Err(StorageError::Failed(
                            "mmap not available after create".to_string(),
                        ));
                    }
                }
            }
        };

        mmap.update_region(offset, data)?;
        Ok(())
    }
}

impl MmapDriver {
    /// Path the new generation of a re-download is written to, kept separate
    /// from the committed file so the published RO snapshot is never aliased.
    /// [`commit`](DriverIo::commit) renames it onto `self.path` atomically.
    fn rewrite_temp_path(&self) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(".kithara-rewrite");
        PathBuf::from(name)
    }
}
