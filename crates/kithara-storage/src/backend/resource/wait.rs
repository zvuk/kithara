#![forbid(unsafe_code)]

use std::ops::Range;

use kithara_platform::{
    thread::yield_now,
    time::{Duration as PlatformDuration, Instant},
};
use kithara_test_utils::kithara;
use tracing::debug;

use crate::{
    StorageError, StorageResult,
    backend::{resource::state::Resource, traits::DriverIo},
    resource::{WaitOutcome, range_covered_by},
};

impl<D: DriverIo> Resource<D> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara::hang_watchdog]
    pub(super) fn wait_range_inner(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        const WAIT_SPIN_TIMEOUT_MS: u64 = 50;

        if range.start > range.end {
            return Err(StorageError::InvalidRange {
                start: range.start,
                end: range.end,
            });
        }

        if range.is_empty() {
            return Ok(WaitOutcome::Ready);
        }

        loop {
            hang_tick!();
            if self.inner.driver.try_fast_check(&range) {
                return Ok(WaitOutcome::Ready);
            }

            let state = self.inner.state.lock_sync();

            if self.inner.cancel.is_cancelled() {
                return Err(StorageError::Cancelled);
            }

            if let Some(ref reason) = state.failed {
                return Err(StorageError::Failed(reason.clone()));
            }

            if range_covered_by(&state.available, &range) {
                return Ok(WaitOutcome::Ready);
            }

            if state.committed {
                let final_len = state.final_len.unwrap_or(0);
                if range.start >= final_len {
                    return Ok(WaitOutcome::Eof);
                }
                let clamped = range.start..range.end.min(final_len);
                if range_covered_by(&state.available, &clamped) {
                    return Ok(WaitOutcome::Ready);
                }
                if self.inner.driver.valid_window().is_none() {
                    return Ok(WaitOutcome::Ready);
                }
            }

            debug!(
                range_start = range.start,
                range_end = range.end,
                committed = state.committed,
                final_len = ?state.final_len,
                "storage::wait_range spinning"
            );

            yield_now();
            let deadline = Instant::now() + PlatformDuration::from_millis(WAIT_SPIN_TIMEOUT_MS);
            let (_state, _wait_result) = self.inner.condvar.wait_sync_timeout(state, deadline);
        }
    }
}
