#![forbid(unsafe_code)]

use std::{ops::Range, path::Path};

use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::acquisition::{RawWriteHandle, ReadSide, WriteSide};

/// Base-layer writer over a `kithara_storage::StorageResource`.
///
/// The decrypt-readiness typestate lives one layer up (in `ProcessedWriter` /
/// `ProcessedReader`). This newtype is the **storage** seam: it carries the
/// write capability of a freshly-acquired (uncommitted) resource and consumes
/// itself on [`commit`](WriteSide::commit) into a [`BaseReader`]. It is not
/// `Clone` — a single producer owns the write side.
#[derive(Debug)]
pub struct BaseWriter(kithara_storage::StorageResource);

/// Base-layer reader over a `kithara_storage::StorageResource`.
///
/// Cheap to clone (the underlying `StorageResource` is `Arc`-backed). Reads see
/// whatever the storage layer has committed; the processed/decrypt gate is
/// applied by `ProcessedReader` above.
#[derive(Clone, Debug)]
pub struct BaseReader(kithara_storage::StorageResource);

impl BaseWriter {
    /// Wrap a freshly-acquired (uncommitted) storage resource.
    pub(crate) fn new(inner: kithara_storage::StorageResource) -> Self {
        Self(inner)
    }
}

impl BaseReader {
    /// Wrap a committed (or in-flight shared) storage resource for reading.
    pub(crate) fn new(inner: kithara_storage::StorageResource) -> Self {
        Self(inner)
    }
}

impl WriteSide for BaseWriter {
    type Reader = BaseReader;

    fn commit(self, final_len: Option<u64>) -> StorageResult<BaseReader> {
        self.0.commit(final_len)?;
        Ok(BaseReader(self.0))
    }

    fn fail(self, reason: String) {
        self.0.fail(reason);
    }

    fn raw_write_handle(&self) -> RawWriteHandle {
        let storage = self.0.clone();
        RawWriteHandle::new(move |offset, data| storage.write_at(offset, data))
    }

    fn reader(&self) -> BaseReader {
        BaseReader(self.0.clone())
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.0.write_at(offset, data)
    }
}

impl ReadSide for BaseReader {
    type Writer = BaseWriter;

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.0.contains_range(range)
    }

    fn len(&self) -> Option<u64> {
        self.0.len()
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.0.next_gap(from, limit)
    }

    fn path(&self) -> Option<&Path> {
        self.0.path()
    }

    fn reactivate(self) -> StorageResult<BaseWriter> {
        self.0.reactivate()?;
        Ok(BaseWriter(self.0))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.0.read_at(offset, buf)
    }

    fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.0.read_inflight_at(offset, buf)
    }

    fn status(&self) -> ResourceStatus {
        self.0.status()
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.0.wait_range(range)
    }
}
