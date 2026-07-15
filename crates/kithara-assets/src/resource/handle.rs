#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::fs;
use std::{
    io::{Error as IoError, ErrorKind},
    ops::Range,
    path::Path,
};

#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::CancelToken;
#[cfg(not(target_arch = "wasm32"))]
use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
use kithara_storage::{ResourceStatus, StorageResource, StorageResult, WaitOutcome};

use super::{RawWriteHandle, ReadSide, WriteSide};

/// Base-layer writer over a `kithara_storage::StorageResource`.
///
/// The decrypt-readiness typestate lives one layer up (in `ProcessedWriter` /
/// `ProcessedReader`). This newtype is the **storage** seam: it carries the
/// write capability of a freshly-acquired (uncommitted) resource and consumes
/// itself on [`commit`](WriteSide::commit) into a [`BaseReader`]. It is not
/// `Clone` — a single producer owns the write side.
#[derive(Debug)]
pub struct BaseWriter(StorageResource);

/// Base-layer reader over a `kithara_storage::StorageResource`.
///
/// Cheap to clone (the underlying `StorageResource` is `Arc`-backed). Reads see
/// whatever the storage layer has committed; the processed/decrypt gate is
/// applied by `ProcessedReader` above.
#[derive(Clone, Debug)]
pub struct BaseReader {
    inner: StorageResource,
    read_only: bool,
}

impl BaseWriter {
    /// Wrap a freshly-acquired (uncommitted) storage resource.
    pub(crate) fn new(inner: StorageResource) -> Self {
        Self(inner)
    }
}

impl BaseReader {
    /// Wrap a committed (or in-flight shared) storage resource for reading.
    pub(crate) fn new(inner: StorageResource) -> Self {
        Self {
            inner,
            read_only: false,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_read_only_file(path: &Path, cancel: &CancelToken) -> StorageResult<Self> {
        fs::metadata(path)?;
        let mmap: MmapResource = Resource::open(
            cancel.clone(),
            MmapOptions::for_path(path.to_path_buf())
                .mode(OpenMode::ReadOnly)
                .build(),
        )?;
        Ok(Self {
            inner: StorageResource::from(mmap),
            read_only: true,
        })
    }
}

impl WriteSide for BaseWriter {
    type Reader = BaseReader;

    fn commit(self, final_len: Option<u64>) -> StorageResult<BaseReader> {
        self.0.commit(final_len)?;
        Ok(BaseReader::new(self.0))
    }

    delegate::delegate! {
        to self.0 {
            fn fail(self, reason: String);
            #[expr(BaseReader::new($))]
            #[call(clone)]
            fn reader(&self) -> BaseReader;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }

    fn raw_write_handle(&self) -> RawWriteHandle {
        let storage = self.0.clone();
        RawWriteHandle::new(move |offset, data| storage.write_at(offset, data))
    }
}

impl ReadSide for BaseReader {
    type Writer = BaseWriter;

    delegate::delegate! {
        to self.inner {
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn len(&self) -> Option<u64>;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
            fn path(&self) -> Option<&Path>;
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn status(&self) -> ResourceStatus;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
        }
    }

    fn reactivate(self) -> StorageResult<BaseWriter> {
        if self.read_only {
            return Err(IoError::new(
                ErrorKind::PermissionDenied,
                "read-only resource cannot be reactivated",
            )
            .into());
        }
        self.inner.reactivate()?;
        Ok(BaseWriter(self.inner))
    }
}
