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
