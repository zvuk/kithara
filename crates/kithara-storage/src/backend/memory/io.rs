#![forbid(unsafe_code)]

use std::path::Path;

use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;

use crate::{
    StorageError, StorageResult,
    backend::{memory::driver::MemDriver, traits::DriverIo},
};

impl DriverIo for MemDriver {
    /// Publishes the snapshot and frees the working buffer atomically under the state lock, so a
    /// concurrent `read_at` never observes a freed buffer as if it held data. A zero-length commit
    /// publishes no snapshot, matching the mmap `Empty` contract.
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        let mut state = self.state.lock();
        let end = final_len.unwrap_or(state.len).min(state.len);
        let end_usize = usize::try_from(end).map_err(|err| {
            StorageError::Failed(format!(
                "memory commit: len {end} does not fit usize: {err}"
            ))
        })?;
        if end_usize > state.buf.len() {
            return Ok(());
        }
        let displaced = if end_usize == 0 {
            self.committed.swap(None)
        } else {
            let snapshot = state.buf[..end_usize].to_vec();
            self.committed.swap(Some(Arc::new(snapshot)))
        };
        state.buf.renew();
        state.len = end;
        drop(state);
        let mut retired = self.retired.lock();
        if let Some(displaced) = displaced {
            retired.retire(displaced);
        } else {
            retired.collect();
        }
        drop(retired);
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

    /// Repopulates the working buffer from the committed snapshot so a re-download can rewrite
    /// incrementally on top of the prior generation, while the snapshot stays published for
    /// lock-free reads until the next `commit` swaps it.
    fn reactivate(&self) -> StorageResult<()> {
        if let Some(snapshot) = self.committed.load_full() {
            let mut state = self.state.lock();
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
        Ok(())
    }

    /// Reads the working buffer first while a generation is in flight, since the writer's own
    /// read-back must observe the current bytes, not the stale published snapshot; once `commit`
    /// frees the buffer, the snapshot becomes authoritative.
    #[kithara::measure]
    fn read_at(&self, offset: u64, buf: &mut [u8], _effective_len: u64) -> StorageResult<usize> {
        let state = self.state.lock();

        let start = usize::try_from(offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read: offset {offset} does not fit usize: {err}"
            ))
        })?;
        let logical_end = usize::try_from(state.len)
            .unwrap_or(usize::MAX)
            .min(state.buf.len());
        if start < logical_end {
            let to_read = buf.len().min(logical_end - start);
            buf[..to_read].copy_from_slice(&state.buf[start..start + to_read]);
            drop(state);
            return Ok(to_read);
        }
        drop(state);

        if let Some(snapshot) = self.committed.load().as_ref() {
            return Self::read_slice(snapshot.as_slice(), offset, buf);
        }
        Ok(0)
    }

    fn read_committed(&self, offset: u64, buf: &mut [u8]) -> StorageResult<Option<usize>> {
        let snapshot = self.committed.load();
        let Some(data) = snapshot.as_ref() else {
            return Ok(None);
        };
        Self::read_slice(data.as_slice(), offset, buf).map(Some)
    }

    fn storage_len(&self) -> u64 {
        let state = self.state.lock();
        state.len
    }

    #[kithara::measure]
    fn write_at(&self, offset: u64, data: &[u8], committed: bool) -> StorageResult<()> {
        if committed {
            return Err(StorageError::Failed(
                "cannot write to committed resource".to_string(),
            ));
        }

        let mut state = self.state.lock();
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

impl MemDriver {
    /// Copy `min(buf.len(), data.len() - offset)` bytes from `data` at `offset`
    /// into `buf`, returning the count. Shared by the committed branch of
    /// `read_at` and by `read_committed`.
    fn read_slice(data: &[u8], offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        let len = u64::try_from(data.len()).map_err(|err| {
            StorageError::Failed(format!(
                "memory read: snapshot len {} does not fit u64: {err}",
                data.len()
            ))
        })?;
        if offset >= len {
            return Ok(0);
        }
        let available = usize::try_from(len - offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read: available {} does not fit usize: {err}",
                len - offset
            ))
        })?;
        let to_read = buf.len().min(available);
        let start = usize::try_from(offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory read: offset {offset} does not fit usize: {err}"
            ))
        })?;
        buf[..to_read].copy_from_slice(&data[start..start + to_read]);
        Ok(to_read)
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };

    use kithara_platform::sync::Arc;

    use crate::{
        backend::{
            memory::driver::{MemDriver, MemOptions},
            traits::{Driver, DriverIo},
        },
        test_pools::{byte_buffer, pools},
    };

    #[kithara::test]
    fn committed_snapshot_from_initial_data() {
        let pools = pools();
        let (driver, _state) = MemDriver::open(
            MemOptions::builder()
                .buffer(byte_buffer(&pools))
                .initial_data(b"hello world".to_vec())
                .build(),
        )
        .expect("open with initial data must succeed");

        assert_eq!(driver.committed_len(), Some(11));

        let mut buf = [0u8; 11];
        let n = driver
            .read_committed(0, &mut buf)
            .expect("read_committed must not error")
            .expect("snapshot is published");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");

        let mut buf6 = [0u8; 5];
        let n = driver
            .read_committed(6, &mut buf6)
            .expect("read_committed must not error")
            .expect("snapshot is published");
        assert_eq!(n, 5);
        assert_eq!(&buf6, b"world");
    }

    /// Regression: `commit` frees the working buffer, so a slow-path `read_at`
    /// racing the commit/reactivate transition must never index a freed buffer.
    /// The publish/free is atomic under the state lock and `read_at` re-checks
    /// `committed` under the same lock; if that ordering regresses this panics
    /// (index out of range) on a reader thread.
    #[kithara::test]
    fn concurrent_commit_reactivate_and_reads_never_panic() {
        let pools = pools();
        let (driver, _state) =
            MemDriver::open(MemOptions::builder().buffer(byte_buffer(&pools)).build())
                .expect("open must succeed");
        driver
            .write_at(0, b"hello world", false)
            .expect("active write must succeed");

        let driver = Arc::new(driver);
        let stop = Arc::new(AtomicBool::new(false));

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let d = Arc::clone(&driver);
                let s = Arc::clone(&stop);
                thread::spawn(move || {
                    while !s.load(Ordering::Relaxed) {
                        let mut buf = [0u8; 11];
                        let n = d
                            .read_at(0, &mut buf, 11)
                            .expect("read_at must not error during transition");
                        assert_eq!(
                            &buf[..n],
                            &b"hello world"[..n],
                            "read_at returned wrong bytes during transition"
                        );
                    }
                })
            })
            .collect();

        // Toggle commit/reactivate to keep reopening the transition window.
        for _ in 0..256 {
            driver.commit(Some(11)).expect("commit must succeed");
            driver.reactivate().expect("reactivate must succeed");
        }
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().expect("reader thread panicked");
        }
    }

    #[kithara::test]
    fn read_committed_without_snapshot_returns_none() {
        let pools = pools();
        let (driver, _state) =
            MemDriver::open(MemOptions::builder().buffer(byte_buffer(&pools)).build())
                .expect("open empty must succeed");

        assert_eq!(driver.committed_len(), None);

        let mut buf = [0u8; 4];
        let read = driver
            .read_committed(0, &mut buf)
            .expect("read_committed must not error without a snapshot");
        assert_eq!(
            read, None,
            "no snapshot must report None (caller uses slow path)"
        );
    }

    #[kithara::test]
    fn commit_frees_working_buffer_and_reactivate_restores_it() {
        let pools = pools();
        let (driver, _state) =
            MemDriver::open(MemOptions::builder().buffer(byte_buffer(&pools)).build())
                .expect("open empty must succeed");

        driver
            .write_at(0, b"hello world", false)
            .expect("active write must succeed");
        assert_eq!(driver.state.lock().buf.len(), 11);

        driver.commit(Some(11)).expect("commit must succeed");

        // The committed bytes live in the snapshot, not the working buffer: the
        // working buffer is released back to the pool (replaced by an empty one),
        // so it no longer holds a second copy. (Asserting `len`, not `capacity`,
        // since the pool can retain the released capacity for reuse.)
        assert_eq!(
            driver.state.lock().buf.len(),
            0,
            "working buffer not released on commit"
        );

        assert_eq!(driver.committed_len(), Some(11));

        let mut buf = [0u8; 11];
        let n = driver
            .read_committed(0, &mut buf)
            .expect("read_committed must not error")
            .expect("snapshot is published");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");

        let mut buf2 = [0u8; 11];
        let n = driver
            .read_at(0, &mut buf2, 11)
            .expect("snapshot-aware read_at must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf2, b"hello world");

        driver.reactivate().expect("reactivate must succeed");

        // The snapshot stays PUBLISHED across reactivate: reads keep serving the
        // immutable prior generation (a re-download rewrites the working buffer
        // in place, so exposing it would risk a torn read). The lifecycle flips
        // to active one layer up (`ResourceCore::committed`), not here.
        assert_eq!(driver.committed_len(), Some(11));
        assert!(
            driver.state.lock().buf.len() >= 11,
            "working buffer not repopulated on reactivate"
        );

        let mut buf3 = [0u8; 11];
        let n = driver
            .read_at(0, &mut buf3, 11)
            .expect("snapshot read_at after reactivate must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf3, b"hello world");
    }

    #[kithara::test]
    fn zero_length_committed_publishes_no_snapshot() {
        let pools = pools();
        // Empty initial data: committed but length 0 → no snapshot (matches mmap `Empty`).
        let (driver, _state) = MemDriver::open(
            MemOptions::builder()
                .buffer(byte_buffer(&pools))
                .initial_data(Vec::new())
                .build(),
        )
        .expect("open with empty initial data must succeed");
        assert_eq!(driver.committed_len(), None);

        // commit(Some(0)) on an empty buffer likewise publishes no snapshot.
        let (driver, _state) =
            MemDriver::open(MemOptions::builder().buffer(byte_buffer(&pools)).build())
                .expect("open empty must succeed");
        driver.commit(Some(0)).expect("commit(0) must succeed");
        assert_eq!(driver.committed_len(), None);
    }
}
