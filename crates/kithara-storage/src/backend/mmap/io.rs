#![forbid(unsafe_code)]

//! [`DriverIo`] impl for [`MmapDriver`]: read/write/commit/reactivate
//! plus the lock-free fast-path queue (`try_fast_check`/`notify_write`).

use std::{
    fs::{self, OpenOptions},
    ops::Range,
    path::Path,
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

        if let Some(len) = final_len {
            if len > 0 {
                // Check if truncation is needed before dropping the mmap.
                let needs_truncate = matches!(
                    &*mmap_guard,
                    MmapState::Active(mmap) if len < mmap.len()
                );

                // Drop old mmap first — avoids SIGBUS from resizing a stale
                // mmap (e.g. after atomic write-rename replaces the backing file).
                *mmap_guard = MmapState::Empty;

                if needs_truncate {
                    // Truncate file to final size via ftruncate (no mmap involved).
                    // After atomic rename, the file already has the correct size
                    // so this branch is skipped.
                    let file_len = fs::metadata(&self.path).map_or(0, |m| m.len());
                    if file_len > len {
                        let f = OpenOptions::new()
                            .write(true)
                            .open(&self.path)
                            .map_err(|e| StorageError::Failed(format!("truncate open: {e}")))?;
                        f.set_len(len)
                            .map_err(|e| StorageError::Failed(format!("truncate: {e}")))?;
                    }
                }

                let ro = MemoryMappedFile::open_ro(&self.path)?;
                *mmap_guard = MmapState::Committed(ro);
            } else {
                *mmap_guard = MmapState::Empty;
                let _ = fs::write(&self.path, b"");
            }
        } else {
            let is_active = matches!(*mmap_guard, MmapState::Active(_));
            if is_active {
                *mmap_guard = MmapState::Empty;
                if self.path.exists() && fs::metadata(&self.path).is_ok_and(|m| m.len() > 0) {
                    let ro = MemoryMappedFile::open_ro(&self.path)?;
                    *mmap_guard = MmapState::Committed(ro);
                }
            }
        }

        drop(mmap_guard);
        Ok(())
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
            MmapState::Active(_) => {}
            MmapState::Committed(_) | MmapState::Empty => {
                *mmap_guard = MmapState::Empty;
                if self.path.exists() && fs::metadata(&self.path).is_ok_and(|m| m.len() > 0) {
                    let rw = MemoryMappedFile::open_rw(&self.path)?;
                    *mmap_guard = MmapState::Active(rw);
                }
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

    fn storage_len(&self) -> u64 {
        let mmap_guard = self.mmap.lock_sync();
        mmap_guard.len()
    }

    fn try_fast_check(&self, range: &Range<u64>) -> bool {
        let mut found_match = false;
        while let Some(ready) = self.ready_ranges.pop() {
            if ready.start <= range.start && ready.end >= range.end {
                found_match = true;
                break;
            }
        }
        found_match
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        let end = offset + data.len() as u64;
        let mut mmap_guard = self.mmap.lock_sync();

        // Handle writes when common state is committed.
        //
        // The `committed` flag is a hint from common state — the driver decides
        // whether the underlying storage can accept writes.
        //
        // - `Committed` + `ReadWrite`: reopen as rw (index files rewritten in place).
        // - `Active` + any mode: already writable, allow the write.
        // - `Empty` + non-ReadOnly: no backing data to protect, fall through to
        //   creation logic below (handles zero-length commit → resume write).
        // - Otherwise: reject (use `reactivate()` to resume writing).
        if committed {
            match (&*mmap_guard, self.mode) {
                (MmapState::Committed(_), OpenMode::ReadWrite) => {
                    *mmap_guard = MmapState::Empty;
                    let rw = MemoryMappedFile::open_rw(&self.path)?;
                    *mmap_guard = MmapState::Active(rw);
                }
                (MmapState::Active(_), _)
                | (MmapState::Empty, OpenMode::Auto | OpenMode::ReadWrite) => {
                    // Already active or zero-length committed — ok to proceed.
                }
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
