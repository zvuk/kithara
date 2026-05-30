#![forbid(unsafe_code)]

use crate::{
    StorageError, StorageResult,
    backend::{resource::state::ResourceCore, traits::DriverIo},
};

impl<D: DriverIo> ResourceCore<D> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(super) fn read_at_inner(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Lock-free committed fast path: a committed resource exposes an
        // immutable snapshot (`committed_len()` is the single source of truth);
        // read straight from it with no state mutex.
        if let Some(committed_len) = self.inner.driver.committed_len() {
            if self.inner.cancel.is_cancelled() {
                return Err(StorageError::Cancelled);
            }
            if offset >= committed_len {
                return Ok(0);
            }
            let available = usize::try_from(committed_len - offset).unwrap_or(usize::MAX);
            let to_read = buf.len().min(available);
            return self
                .inner
                .driver
                .read_committed(offset, &mut buf[..to_read]);
        }

        self.check_health()?;

        let effective_len = {
            let final_len = self.inner.state.lock_sync().final_len;
            let storage_len = self.inner.driver.storage_len();
            let data_len = final_len.unwrap_or(storage_len);
            data_len.min(storage_len)
        };

        if offset >= effective_len {
            return Ok(0);
        }

        let available = usize::try_from(effective_len - offset).unwrap_or(usize::MAX);
        let to_read = buf.len().min(available);

        self.inner
            .driver
            .read_at(offset, &mut buf[..to_read], effective_len)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(super) fn write_at_inner(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        if data.is_empty() {
            return Ok(());
        }

        self.check_health()?;

        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(StorageError::InvalidRange {
                start: offset,
                end: u64::MAX,
            })?;

        let committed = {
            let state = self.inner.state.lock_sync();
            state.committed
        };

        self.inner.driver.write_at(offset, data, committed)?;

        let range = offset..end;
        self.inner.driver.notify_write(&range);

        {
            let mut state = self.inner.state.lock_sync();
            state.available.insert(range.clone());

            if let Some(window) = self.inner.driver.valid_window() {
                if window.start > 0 {
                    state.available.remove(0..window.start);
                }
                let upper = state.final_len.unwrap_or(u64::MAX);
                if window.end < upper {
                    state.available.remove(window.end..upper);
                }
            }
        }
        self.inner.condvar.notify_all();

        if let Some(observer) = self.inner.observer.as_ref() {
            observer.on_write(range);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::sync::mpsc;

    use kithara_platform::{thread, time::Duration};
    use tokio_util::sync::CancellationToken;

    use crate::backend::{
        memory::driver::{MemDriver, MemOptions},
        resource::state::ResourceCore,
        traits::DriverIo,
    };

    /// A committed resource's `read_at_inner` must complete WITHOUT taking the
    /// `inner.state` mutex. The test holds the state mutex on this thread while a
    /// worker thread reads; a lock-free fast path completes immediately, a slow
    /// path blocks on the held guard and times out.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn committed_read_does_not_take_state_mutex() {
        let core: ResourceCore<MemDriver> = ResourceCore::open(
            CancellationToken::new(),
            MemOptions {
                initial_data: Some(b"hello world".to_vec()),
                capacity: 0,
            },
        )
        .expect("BUG: MemDriver::open with initial_data is infallible");

        assert_eq!(core.inner.driver.committed_len(), Some(11));

        let guard = core.inner.state.lock_sync();

        let (tx, rx) = mpsc::channel();
        let worker = core.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 11];
            let result = worker.read_at_inner(0, &mut buf);
            let _ = tx.send(result.map(|n| (n, buf)));
        });

        let received = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("committed read blocked on the state mutex");

        drop(guard);

        let (n, buf) = received.expect("committed read failed");
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }
}
