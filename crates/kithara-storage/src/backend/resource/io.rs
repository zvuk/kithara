#![forbid(unsafe_code)]

//! `Resource<D>::read_at_inner` and `write_at_inner` — the read/write
//! hot-path bodies for the [`ResourceExt`](crate::ResourceExt) impl.
//! Methods are ordered alphabetically per `trait_item_order`.

use crate::{
    StorageError, StorageResult,
    backend::{resource::state::Resource, traits::DriverIo},
};

impl<D: DriverIo> Resource<D> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub(super) fn read_at_inner(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        if buf.is_empty() {
            return Ok(0);
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

        // Clamp buf to effective_len.
        #[expect(clippy::cast_possible_truncation)] // byte offset within allocated resource
        let available = (effective_len - offset) as usize;
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

        // Notify fast-path (mmap: push to lock-free queue).
        let range = offset..end;
        self.inner.driver.notify_write(&range);

        // Update common state and wake waiters.
        {
            let mut state = self.inner.state.lock_sync();
            state.available.insert(range.clone());

            // Invalidate evicted ranges for ring buffer drivers.
            if let Some(window) = self.inner.driver.valid_window() {
                if window.start > 0 {
                    state.available.remove(0..window.start);
                }
                // Also evict data above window end (backward seek case).
                // For ring buffers, data outside the valid window is overwritten.
                let upper = state.final_len.unwrap_or(u64::MAX);
                if window.end < upper {
                    state.available.remove(window.end..upper);
                }
            }
        }
        self.inner.condvar.notify_all();

        // Observer fires outside the state lock — implementations are
        // free to take their own locks without deadlocking against
        // `wait_range` waiters on the same resource.
        if let Some(observer) = self.inner.observer.as_ref() {
            observer.on_write(range);
        }

        Ok(())
    }
}
