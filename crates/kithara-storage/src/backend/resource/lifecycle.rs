#![forbid(unsafe_code)]

//! `Resource<D>` lifecycle + inspection bodies: `commit_inner`,
//! `fail_inner`, `reactivate_inner`, plus the read-only inspectors
//! (`path_inner`, `len_inner`, `status_inner`, `contains_range_inner`,
//! `next_gap_inner`).

use std::{ops::Range, path::Path};

use crate::{
    StorageResult,
    backend::{resource::state::Resource, traits::DriverIo},
    resource::{ResourceStatus, range_covered_by},
};

impl<D: DriverIo> Resource<D> {
    pub(super) fn commit_inner(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;

        // Driver-specific commit first (may fail).
        self.inner.driver.commit(final_len)?;

        // Update common state only on success.
        {
            let mut state = self.inner.state.lock_sync();
            state.committed = true;
            state.final_len = final_len;
            if let Some(len) = final_len
                && len > 0
            {
                state.available.insert(0..len);
                // For ring buffer drivers, only data in the valid window is
                // actually available. Remove evicted ranges.
                if let Some(window) = self.inner.driver.valid_window() {
                    if window.start > 0 {
                        state.available.remove(0..window.start);
                    }
                    if window.end < len {
                        state.available.remove(window.end..len);
                    }
                }
            }
        }
        self.inner.condvar.notify_all();

        // Observer fires outside the state lock and only when the
        // caller supplied a final length — `commit(None)` is silent.
        if let Some(len) = final_len
            && let Some(observer) = self.inner.observer.as_ref()
        {
            observer.on_commit(len);
        }

        Ok(())
    }

    pub(super) fn contains_range_inner(&self, range: Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        let state = self.inner.state.lock_sync();
        range_covered_by(&state.available, &range)
    }

    pub(super) fn fail_inner(&self, reason: String) {
        {
            let mut state = self.inner.state.lock_sync();
            state.failed = Some(reason);
        }
        self.inner.condvar.notify_all();
    }

    pub(super) fn len_inner(&self) -> Option<u64> {
        let state = self.inner.state.lock_sync();
        state.final_len
    }

    pub(super) fn next_gap_inner(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        let state = self.inner.state.lock_sync();
        let total = state.final_len.unwrap_or(limit);
        let upper = limit.min(total);
        if from >= upper {
            return None;
        }
        state
            .available
            .gaps(&(from..upper))
            .next()
            .map(|gap| gap.start..gap.end.min(upper))
    }

    pub(super) fn path_inner(&self) -> Option<&Path> {
        self.inner.driver.path()
    }

    pub(super) fn reactivate_inner(&self) -> StorageResult<()> {
        self.check_health()?;

        // Driver-specific reactivation first (may fail).
        self.inner.driver.reactivate()?;

        // Update common state only on success.
        {
            let mut state = self.inner.state.lock_sync();
            state.committed = false;
            state.final_len = None;
            // Keep available data as-is — existing bytes remain readable.
        }
        self.inner.condvar.notify_all();
        Ok(())
    }

    pub(super) fn status_inner(&self) -> ResourceStatus {
        let state = self.inner.state.lock_sync();
        if let Some(ref reason) = state.failed {
            // `Failed` keeps priority over `Cancelled` — a data error
            // is more informative than a routine shutdown signal.
            ResourceStatus::Failed(reason.clone())
        } else if state.committed {
            // `Committed` keeps priority too: bytes are still
            // readable for observers that opened a fresh handle on
            // top of an already-committed file.
            ResourceStatus::Committed {
                final_len: state.final_len,
            }
        } else if self.inner.cancel.is_cancelled() {
            // Token-fired before the data lifecycle progressed.
            // Surface as `Cancelled` so blocking observers (e.g.
            // `kithara_assets::ProcessedResource`'s readiness gate)
            // can wake immediately instead of polling on a watchdog.
            ResourceStatus::Cancelled
        } else {
            ResourceStatus::Active
        }
    }
}
