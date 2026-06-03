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
    backend::{resource::state::ResourceCore, traits::DriverIo},
    resource::{WaitOutcome, range_covered_by},
};

/// Watchdog timeout for the network-bound `wait_range_inner`: must exceed
/// the `kithara-net` `total_timeout` (default 120s) so a stalled upstream is
/// failed by the network layer (this wait then returns `Failed`) before the
/// deadlock-watchdog fires. Only a wait that never returns after the fetch
/// resolved is a real deadlock.
const WAIT_HANG_TIMEOUT: kithara_platform::time::Duration =
    kithara_platform::time::Duration::from_secs(180);

impl<D: DriverIo> ResourceCore<D> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara::hang_watchdog(timeout = WAIT_HANG_TIMEOUT)]
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

        // How far the available prefix of `range` reaches. Bytes arrive
        // front-to-back for a sequential fetch, so this advancing means the
        // wait is making progress (not deadlocked) and the watchdog resets.
        let mut filled_front = range.start;

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

            let front = state
                .available
                .gaps(&range)
                .next()
                .map_or(range.end, |gap| gap.start);
            if front > filled_front {
                filled_front = front;
                hang_reset!();
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
            let _state = self.inner.condvar.wait_sync_timeout(state, deadline);
        }
    }
}
