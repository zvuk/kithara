#![forbid(unsafe_code)]

//! [`DriverIo`] impl for [`MemDriver`]: read/write/commit/reactivate
//! operations against the in-memory pooled buffer.

use std::path::Path;

use crate::{
    StorageError, StorageResult,
    backend::{memory::driver::MemDriver, traits::DriverIo},
};

impl DriverIo for MemDriver {
    fn commit(&self, _final_len: Option<u64>) -> StorageResult<()> {
        Ok(())
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn reactivate(&self) -> StorageResult<()> {
        Ok(())
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        let state = self.state.lock_sync();

        if offset >= state.len {
            return Ok(0);
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to len which fits in memory"
        )]
        let available = (state.len - offset) as usize;
        let to_read = buf.len().min(available);

        #[expect(
            clippy::cast_possible_truncation,
            reason = "offset < len which fits in memory"
        )]
        let start = offset as usize;
        buf[..to_read].copy_from_slice(&state.buf[start..start + to_read]);
        drop(state);

        Ok(to_read)
    }

    fn storage_len(&self) -> u64 {
        let state = self.state.lock_sync();
        state.len
    }

    // valid_window() returns None (default) — no eviction, all data retained.

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        if committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        let mut state = self.state.lock_sync();
        let end = offset + data.len() as u64;

        #[expect(
            clippy::cast_possible_truncation,
            reason = "bounded by byte budget (256 MB)"
        )]
        let end_usize = end as usize;

        // Grow buffer if write extends beyond current allocation.
        if end_usize > state.buf.len() {
            state
                .buf
                .ensure_len(end_usize)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "offset < end which fits in memory"
        )]
        let start = offset as usize;
        state.buf[start..end_usize].copy_from_slice(data);

        if end > state.len {
            state.len = end;
        }
        drop(state);

        Ok(())
    }
}
