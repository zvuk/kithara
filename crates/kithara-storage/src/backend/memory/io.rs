#![forbid(unsafe_code)]

use std::{path::Path, sync::Arc};

use crate::{
    StorageError, StorageResult,
    backend::{memory::driver::MemDriver, traits::DriverIo},
};

impl DriverIo for MemDriver {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let mut state = self.state.lock_sync();
        let end = final_len.unwrap_or(state.len).min(state.len);
        let end_usize = usize::try_from(end).map_err(|err| {
            StorageError::Failed(format!(
                "memory commit: len {end} does not fit usize: {err}"
            ))
        })?;
        if end_usize == 0 {
            // Release the working buffer's heap; the committed snapshot is the
            // single source of committed bytes. `shrink_to_fit` actually frees
            // (plain `clear` would retain the capacity).
            state.buf.clear();
            state.buf.shrink_to_fit();
            drop(state);
            // Zero-length committed: no snapshot (matches the mmap `Empty`
            // contract — `committed_len()` reports `None`, reads use the slow path).
            self.committed.store(None);
            return Ok(());
        }
        // Snapshot the committed bytes first, then release the working buffer's
        // heap so committed mem resources hold a single copy (the snapshot).
        let snapshot = state.buf[..end_usize].to_vec();
        state.buf.clear();
        state.buf.shrink_to_fit();
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
        // Repopulate the working buffer (released on commit) and set `state.len`
        // while `committed` is still `Some`, so concurrent reads keep taking the
        // snapshot fast path. Only then clear the snapshot, after which reads
        // fall to the slow path and find the repopulated buffer. Do NOT reorder.
        if let Some(snapshot) = self.committed.load_full() {
            let mut state = self.state.lock_sync();
            let snap_len = snapshot.len();
            state
                .buf
                .ensure_len(snap_len)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
            state.buf[..snap_len].copy_from_slice(&snapshot);
            state.len = u64::try_from(snap_len).map_err(|err| {
                StorageError::Failed(format!(
                    "memory reactivate: len {snap_len} does not fit u64: {err}"
                ))
            })?;
            drop(state);
        }
        self.committed.store(None);
        Ok(())
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        // A committed resource's working buffer has been released; serve from the
        // immutable snapshot. This also covers a commit that completed while this
        // read was in flight (the fast path checked `committed_len()` before the
        // snapshot was published), preventing a read against the freed buffer.
        if self.committed.load().is_some() {
            return self.read_committed(offset, buf);
        }

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
    fn commit_frees_working_buffer_and_reactivate_restores_it() {
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
        })
        .expect("open empty must succeed");

        driver
            .write_at(0, b"hello world", false)
            .expect("active write must succeed");

        let cap_before = driver.state.lock_sync().buf.capacity();
        assert!(
            cap_before >= 11,
            "expected capacity >= 11, got {cap_before}"
        );

        driver.commit(Some(11)).expect("commit must succeed");

        let cap_after = driver.state.lock_sync().buf.capacity();
        assert!(
            cap_after < cap_before,
            "working buffer not released on commit (cap_before={cap_before}, cap_after={cap_after})"
        );
        assert_eq!(cap_after, 0, "shrink_to_fit must reach capacity 0");

        assert_eq!(driver.committed_len(), Some(11));

        let mut buf = [0u8; 11];
        let n = driver
            .read_committed(0, &mut buf)
            .expect("read_committed from snapshot must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");

        let mut buf2 = [0u8; 11];
        let n = driver
            .read_at(0, &mut buf2, 11)
            .expect("snapshot-aware read_at must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf2, b"hello world");

        driver.reactivate().expect("reactivate must succeed");

        assert_eq!(driver.committed_len(), None);
        assert!(
            driver.state.lock_sync().buf.len() >= 11,
            "working buffer not repopulated on reactivate"
        );

        let mut buf3 = [0u8; 11];
        let n = driver
            .read_at(0, &mut buf3, 11)
            .expect("slow-path read_at after reactivate must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf3, b"hello world");
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
