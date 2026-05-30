#![forbid(unsafe_code)]

use std::{path::Path, sync::Arc};

use crate::{
    StorageError, StorageResult,
    backend::{memory::driver::MemDriver, traits::DriverIo},
};

impl DriverIo for MemDriver {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let state = self.state.lock_sync();
        let end = final_len.unwrap_or(state.len).min(state.len);
        let end_usize = usize::try_from(end).map_err(|err| {
            StorageError::Failed(format!(
                "memory commit: len {end} does not fit usize: {err}"
            ))
        })?;
        if end_usize == 0 {
            drop(state);
            // Zero-length committed: no snapshot (matches the mmap `Empty`
            // contract — `committed_len()` reports `None`, reads use the slow path).
            self.committed.store(None);
            return Ok(());
        }
        let snapshot = state.buf[..end_usize].to_vec();
        drop(state);
        self.committed.store(Some(Arc::new(snapshot)));
        Ok(())
    }

    fn committed_len(&self) -> Option<u64> {
        self.committed.load().as_ref().and_then(|v| {
            let len = v.len();
            u64::try_from(len).ok()
        })
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn reactivate(&self) -> StorageResult<()> {
        self.committed.store(None);
        Ok(())
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        let state = self.state.lock_sync();

        if offset >= state.len {
            return Ok(0);
        }

        let available = usize::try_from(state.len - offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read: available {} does not fit usize: {err}",
                state.len - offset
            ))
        })?;
        let to_read = buf.len().min(available);

        let start = usize::try_from(offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read: offset {offset} does not fit usize: {err}"
            ))
        })?;
        buf[..to_read].copy_from_slice(&state.buf[start..start + to_read]);
        drop(state);

        Ok(to_read)
    }

    fn read_committed(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        let snapshot = self.committed.load();
        let Some(data) = snapshot.as_ref() else {
            return Err(StorageError::Failed(
                "read_committed without a published committed snapshot".into(),
            ));
        };

        let len = u64::try_from(data.len()).map_err(|err| {
            StorageError::Failed(format!(
                "memory read_committed: len {} does not fit u64: {err}",
                data.len()
            ))
        })?;
        if offset >= len {
            return Ok(0);
        }

        let available = usize::try_from(len - offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read_committed: available {} does not fit usize: {err}",
                len - offset
            ))
        })?;
        let to_read = buf.len().min(available);

        let start = usize::try_from(offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read_committed: offset {offset} does not fit usize: {err}"
            ))
        })?;
        buf[..to_read].copy_from_slice(&data[start..start + to_read]);

        Ok(to_read)
    }

    fn storage_len(&self) -> u64 {
        let state = self.state.lock_sync();
        state.len
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        if committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        let mut state = self.state.lock_sync();
        let end = offset + data.len() as u64;

        let end_usize = usize::try_from(end).map_err(|err| {
            StorageError::Failed(format!("memory write: end {end} does not fit usize: {err}"))
        })?;

        if end_usize > state.buf.len() {
            state
                .buf
                .ensure_len(end_usize)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
        }

        let start = usize::try_from(offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory write: offset {offset} does not fit usize: {err}"
            ))
        })?;
        state.buf[start..end_usize].copy_from_slice(data);

        if end > state.len {
            state.len = end;
        }
        drop(state);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use crate::{
        StorageError,
        backend::{
            memory::driver::{MemDriver, MemOptions},
            traits::{Driver, DriverIo},
        },
    };

    #[kithara::test]
    fn committed_snapshot_from_initial_data() {
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: Some(b"hello world".to_vec()),
            capacity: 0,
        })
        .expect("open with initial data must succeed");

        assert_eq!(driver.committed_len(), Some(11));

        let mut buf = [0u8; 11];
        let n = driver
            .read_committed(0, &mut buf)
            .expect("read_committed from published snapshot must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");

        let mut buf6 = [0u8; 5];
        let n = driver
            .read_committed(6, &mut buf6)
            .expect("mid-offset read_committed must succeed");
        assert_eq!(n, 5);
        assert_eq!(&buf6, b"world");
    }

    #[kithara::test]
    fn read_committed_without_snapshot_errors() {
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
        })
        .expect("open empty must succeed");

        assert_eq!(driver.committed_len(), None);

        let mut buf = [0u8; 4];
        let err = driver
            .read_committed(0, &mut buf)
            .expect_err("read_committed without a snapshot must error");
        assert!(matches!(err, StorageError::Failed(_)));
    }

    #[kithara::test]
    fn zero_length_committed_publishes_no_snapshot() {
        // Empty initial data: committed but length 0 → no snapshot (matches mmap `Empty`).
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: Some(Vec::new()),
            capacity: 0,
        })
        .expect("open with empty initial data must succeed");
        assert_eq!(driver.committed_len(), None);

        // commit(Some(0)) on an empty buffer likewise publishes no snapshot.
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
        })
        .expect("open empty must succeed");
        driver.commit(Some(0)).expect("commit(0) must succeed");
        assert_eq!(driver.committed_len(), None);
    }
}
