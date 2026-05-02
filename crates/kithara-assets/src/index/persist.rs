#![forbid(unsafe_code)]

//! Shared lazy-init helper for disk-backed pin/lru indexes.
//!
//! Both `PinsIndex` and `LruIndex` materialise their on-disk file
//! lazily on the first flush. The same race-tolerant init pattern is
//! used in both, factored here to avoid drift.

use std::{path::Path, sync::OnceLock};

use kithara_storage::{Atomic, MmapOptions, MmapResource, OpenMode, Resource, StorageError};
use tokio_util::sync::CancellationToken;

use crate::error::{AssetsError, AssetsResult};

/// Initial mmap capacity used when materialising an on-disk index file.
///
/// Same as the historical `Atomic` default — large enough to hold a
/// few thousand entries before a remap, small enough that the sparse
/// file footprint is trivial when the cache is idle.
pub(super) const INITIAL_LEN: u64 = 4096;

/// Open an existing on-disk index file in read-write mode without
/// resizing it. Used when a file is detected on construction so its
/// contents can be hydrated.
pub(super) fn open_existing(path: &Path, cancel: &CancellationToken) -> AssetsResult<MmapResource> {
    let res: MmapResource = Resource::open(
        cancel.clone(),
        MmapOptions::new(path.to_path_buf()).with_mode(OpenMode::ReadWrite),
    )?;
    Ok(res)
}

/// Open or create an on-disk index file in read-write mode with the
/// canonical [`INITIAL_LEN`] sparse footprint. Used on first flush
/// when no pre-existing file was hydrated.
pub(super) fn open_for_write(
    path: &Path,
    cancel: &CancellationToken,
) -> AssetsResult<MmapResource> {
    let res: MmapResource = Resource::open(
        cancel.clone(),
        MmapOptions::new(path.to_path_buf())
            .with_initial_len(INITIAL_LEN)
            .with_mode(OpenMode::ReadWrite),
    )?;
    Ok(res)
}

/// Race-tolerant init for the lazy on-disk handle.
///
/// Returns the inhabitant of `cell`, materialising it via
/// [`open_for_write`] if empty. If two threads race the loser's
/// freshly-opened resource is dropped immediately; the winning
/// resource is returned to both.
pub(super) fn init_atomic<'a>(
    cell: &'a OnceLock<Atomic<MmapResource>>,
    path: &Path,
    cancel: &CancellationToken,
) -> AssetsResult<&'a Atomic<MmapResource>> {
    if let Some(a) = cell.get() {
        return Ok(a);
    }
    let new_atomic = Atomic::new(open_for_write(path, cancel)?);
    let _ = cell.set(new_atomic);
    cell.get().ok_or_else(|| {
        AssetsError::Storage(StorageError::Failed(
            "OnceLock not populated after set".to_string(),
        ))
    })
}
