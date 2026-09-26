#![forbid(unsafe_code)]

use std::ops::Range;

use kithara_platform::{CancelToken, sync::Arc};
use kithara_test_utils::kithara;

use crate::{
    StorageError, StorageResult,
    backend::{resource::state::ResourceCore, traits::DriverIo},
    consts,
    resource::{WaitOutcome, range_covered_by},
};

impl<D: DriverIo> ResourceCore<D> {
    /// Tracks how far the available prefix of the range reaches; since bytes arrive front-to-back
    /// for a sequential fetch, its advance signals progress and resets the hang watchdog. Failing a
    /// fast check, the wait parks until the gate is notified — event-driven, with no timer.
    #[kithara::measure]
    #[kithara::hang_watchdog(timeout = consts::WAIT_HANG_TIMEOUT)]
    pub(super) fn wait_range_inner(
        &self,
        range: Range<u64>,
        wait_cancel: Option<&CancelToken>,
    ) -> StorageResult<WaitOutcome> {
        if range.start > range.end {
            return Err(StorageError::InvalidRange {
                start: range.start,
                end: range.end,
            });
        }

        if range.is_empty() {
            return Ok(WaitOutcome::Ready);
        }

        let _resource_cancel_wake = {
            let inner = Arc::clone(&self.inner);
            self.inner.cancel.on_cancel(move || inner.wake_waiters())
        };
        let _wait_cancel_wake = wait_cancel.map(|cancel| {
            let inner = Arc::clone(&self.inner);
            cancel.on_cancel(move || inner.wake_waiters())
        });

        let mut filled_front = range.start;

        loop {
            hang_tick!();
            if self.inner.driver.try_fast_check(&range) {
                return Ok(WaitOutcome::Ready);
            }

            let state = self.inner.gate.lock();

            if self.inner.cancel.is_cancelled()
                || wait_cancel.is_some_and(CancelToken::is_cancelled)
            {
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

            let front = state
                .available
                .gaps(&range)
                .next()
                .map_or(range.end, |gap| gap.start);
            if front > filled_front {
                filled_front = front;
                hang_reset!();
            }

            let _state = self.inner.gate.wait(state);
        }
    }
}
