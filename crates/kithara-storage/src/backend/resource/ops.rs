#![forbid(unsafe_code)]

//! Thin [`ResourceExt`] impl for [`Resource<D>`]: each method forwards
//! to the corresponding `*_inner` inherent method on the resource. The
//! actual bodies live in `super::state` so this file stays
//! responsibility-free aside from the trait wiring.

use std::{ops::Range, path::Path};

use super::state::Resource;
use crate::{
    StorageResult,
    backend::traits::DriverIo,
    resource::{ResourceExt, ResourceStatus, WaitOutcome},
};

impl<D: DriverIo> ResourceExt for Resource<D> {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.commit_inner(final_len)
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.contains_range_inner(range)
    }

    fn fail(&self, reason: String) {
        self.fail_inner(reason);
    }

    fn len(&self) -> Option<u64> {
        self.len_inner()
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.next_gap_inner(from, limit)
    }

    fn path(&self) -> Option<&Path> {
        self.path_inner()
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.reactivate_inner()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.read_at_inner(offset, buf)
    }

    fn status(&self) -> ResourceStatus {
        self.status_inner()
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.wait_range_inner(range)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.write_at_inner(offset, data)
    }
}
