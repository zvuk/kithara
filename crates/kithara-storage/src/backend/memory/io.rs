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
        // Publish a zero-copy snapshot under the lock: when `end` covers the
        // whole buffer (the common case) share the `Arc`, else snapshot the
        // committed prefix. The working buffer is NOT freed — it and the
        // snapshot share one allocation, so a committed resource holds a single
        // copy. Zero-length committed publishes no snapshot — matching the mmap
        // `Empty` contract (`committed_len()` → `None`).
        if end_usize == 0 {
            self.committed.store(None);
        } else if end_usize == state.buf.len() {
            self.committed.store(Some(Arc::clone(&state.buf)));
        } else {
            // Rare: committed length shorter than the buffer — snapshot the
            // prefix into a fresh pooled buffer so `committed_len()` is exact.
            let mut snap = self.byte_pool.get();
            snap.ensure_len(end_usize)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
            snap[..end_usize].copy_from_slice(&state.buf[..end_usize]);
            self.committed.store(Some(Arc::new(snap)));
        }
        state.len = end;
        drop(state);
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
        // The working buffer is never freed (it holds the data directly), so
        // reactivate only drops the committed snapshot. A subsequent write is
        // copy-on-write (see `write_at`): it copies into a fresh pooled buffer
        // only if a reader still holds the prior snapshot `Arc`, isolating the
        // re-download from in-flight reads.
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

    fn read_committed(&self, offset: u64, buf: &mut [u8]) -> StorageResult<Option<usize>> {
        let snapshot = self.committed.load();
        let Some(data) = snapshot.as_ref() else {
            return Ok(None);
        };
        Self::read_slice(data.as_slice(), offset, buf).map(Some)
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

        let start = usize::try_from(offset).map_err(|err| {
            StorageError::Failed(format!(
                "memory write: offset {offset} does not fit usize: {err}"
            ))
        })?;

        // Copy-on-write: if a snapshot or in-flight read still holds the buffer
        // `Arc`, copy the prior bytes into a fresh pooled buffer so those reads
        // keep the old content while this write builds the new one.
        if Arc::get_mut(&mut state.buf).is_none() {
            let old_len = state.buf.len();
            let mut fresh = self.byte_pool.get();
            fresh
                .ensure_len(old_len)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
            fresh[..old_len].copy_from_slice(&state.buf[..old_len]);
            state.buf = Arc::new(fresh);
        }
        let Some(pooled) = Arc::get_mut(&mut state.buf) else {
            return Err(StorageError::Failed(
                "memory write: buffer not unique after copy-on-write".to_string(),
            ));
        };
        if end_usize > pooled.len() {
            pooled
                .ensure_len(end_usize)
                .map_err(|_| StorageError::Failed("byte budget exhausted".to_string()))?;
        }
        pooled[start..end_usize].copy_from_slice(data);

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
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    use kithara_platform::time::Duration;

    use crate::backend::{
        memory::driver::{MemDriver, MemOptions},
        traits::{Driver, DriverIo},
    };

    #[kithara::test]
    fn committed_snapshot_from_initial_data() {
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: Some(b"hello world".to_vec()),
            capacity: 0,
            ..Default::default()
        })
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
    #[kithara::test(timeout(Duration::from_secs(30)))]
    fn concurrent_commit_reactivate_and_reads_never_panic() {
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
            ..Default::default()
        })
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
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
            ..Default::default()
        })
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
    fn commit_shares_snapshot_arc_and_reactivate_keeps_data() {
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
            ..Default::default()
        })
        .expect("open empty must succeed");

        driver
            .write_at(0, b"hello world", false)
            .expect("active write must succeed");

        driver.commit(Some(11)).expect("commit must succeed");

        // Zero-copy snapshot: the working buffer and the committed snapshot
        // share ONE allocation, so a committed resource holds a single copy.
        let buf_arc = {
            let state = driver.state.lock_sync();
            Arc::clone(&state.buf)
        };
        let snap = driver
            .committed
            .load_full()
            .expect("snapshot must be published");
        assert!(
            Arc::ptr_eq(&buf_arc, &snap),
            "commit must share the buffer Arc with the snapshot (no copy)"
        );
        assert_eq!(driver.committed_len(), Some(11));

        let mut buf = [0u8; 11];
        let n = driver
            .read_committed(0, &mut buf)
            .expect("read_committed must not error")
            .expect("snapshot is published");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");

        // reactivate drops the snapshot but keeps the buffer data intact.
        driver.reactivate().expect("reactivate must succeed");
        assert_eq!(driver.committed_len(), None);

        let mut buf2 = [0u8; 11];
        let n = driver
            .read_at(0, &mut buf2, 11)
            .expect("read_at after reactivate must succeed");
        assert_eq!(n, 11);
        assert_eq!(&buf2, b"hello world");

        // A write after reactivate extends the buffer; prior bytes survive.
        driver
            .write_at(11, b"!", false)
            .expect("write after reactivate must succeed");
        let mut buf3 = [0u8; 12];
        let n = driver
            .read_at(0, &mut buf3, 12)
            .expect("read after re-write must succeed");
        assert_eq!(n, 12);
        assert_eq!(&buf3, b"hello world!");
    }

    #[kithara::test]
    fn zero_length_committed_publishes_no_snapshot() {
        // Empty initial data: committed but length 0 → no snapshot (matches mmap `Empty`).
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: Some(Vec::new()),
            capacity: 0,
            ..Default::default()
        })
        .expect("open with empty initial data must succeed");
        assert_eq!(driver.committed_len(), None);

        // commit(Some(0)) on an empty buffer likewise publishes no snapshot.
        let (driver, _state) = MemDriver::open(MemOptions {
            initial_data: None,
            capacity: 0,
            ..Default::default()
        })
        .expect("open empty must succeed");
        driver.commit(Some(0)).expect("commit(0) must succeed");
        assert_eq!(driver.committed_len(), None);
    }
}
