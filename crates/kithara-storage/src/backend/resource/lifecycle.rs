#![forbid(unsafe_code)]

use std::{ops::Range, path::Path, sync::atomic::Ordering};

use crate::{
    StorageResult,
    backend::{resource::state::ResourceCore, traits::DriverIo},
    resource::{ResourceStatus, range_covered_by},
};

impl<D: DriverIo> ResourceCore<D> {
    /// Release the writer without the anti-hang failure stamp. Idempotent.
    pub(super) fn abandon_inner(&self) {
        self.inner.stamp_on_drop.store(false, Ordering::Release);
    }

    pub(super) fn commit_inner(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;
        self.inner.driver.commit(final_len)?;
        self.notify_commit_inner(final_len);
        self.publish_commit_inner(final_len);
        Ok(())
    }

    /// Called from the decode produce path (`phase_at` cascade). Reads the
    /// snapshot through a guard only: writers retire what they displace, so
    /// dropping the guard never frees a range tree on the audio thread.
    ///
    /// Takes a lock-free fast path once committed: a published snapshot always covers the whole
    /// `[0, committed_len)` since both drivers are linear with no eviction, so coverage reduces to
    /// a single length comparison.
    pub(super) fn contains_range_inner(&self, range: Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        if let Some(committed_len) = self.inner.driver.committed_len() {
            return range.end <= committed_len;
        }
        range_covered_by(&self.inner.available_snapshot.load(), &range)
    }

    pub(super) fn fail_inner(&self, reason: String) {
        {
            let mut state = self.inner.gate.lock();
            state.failed = Some(reason);
        }
        self.inner.gate.notify_all();
    }

    /// Notify the original observer after the canonical backing is readable,
    /// before publishing readiness. Callers release their handover locks first
    /// because an observer may read the resource it is recording.
    pub(super) fn notify_commit_inner(&self, final_len: Option<u64>) {
        if let Some(len) = final_len
            && let Some(observer) = self.inner.observer.as_ref()
        {
            observer.on_commit(len);
        }
    }

    /// Hold a reopened driver's readable snapshot in an active lifecycle until
    /// its original observer has recorded the commit. Unlike reactivation,
    /// this leaves the backing store and available bytes untouched.
    pub(super) fn stage_commit_inner(&self) {
        self.inner.committed.store(false, Ordering::Release);
        let mut state = self.inner.gate.lock();
        state.committed = false;
        state.final_len = None;
    }

    /// Publish readiness after the observer has recorded availability. Whoever
    /// sees `Committed` may evict the resource, so its record must already exist.
    /// The displaced availability snapshot is retired to the write side.
    pub(super) fn publish_commit_inner(&self, final_len: Option<u64>) {
        self.inner.committed.store(true, Ordering::Release);

        {
            let mut state = self.inner.gate.lock();
            state.committed = true;
            state.final_len = final_len;
            if let Some(len) = final_len
                && len > 0
            {
                state.available.insert(0..len);
                if let Some(window) = self.inner.driver.valid_window() {
                    if window.start > 0 {
                        state.available.remove(0..window.start);
                    }
                    if window.end < len {
                        state.available.remove(window.end..len);
                    }
                }
                self.inner.publish_available(state);
            }
        }
        self.inner.gate.notify_all();
    }

    /// The committed snapshot stays published across a `reactivate`, so this confirms the lock-free
    /// lifecycle flag is still committed before trusting the snapshot's length as the resource's
    /// final length.
    pub(super) fn len_inner(&self) -> Option<u64> {
        if self.inner.committed.load(Ordering::Acquire)
            && let Some(committed_len) = self.inner.driver.committed_len()
        {
            return Some(committed_len);
        }
        let state = self.inner.gate.lock();
        state.final_len
    }

    pub(super) fn next_gap_inner(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        let state = self.inner.gate.lock();
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

    /// A new write generation starts armed: `abandon` only waives the anti-hang stamp for the
    /// writer that owns this refill, never for whoever writes next over the same core.
    pub(super) fn reactivate_inner(&self) -> StorageResult<()> {
        if self.inner.cancel.is_cancelled() {
            return Err(crate::StorageError::Cancelled);
        }

        self.inner.driver.reactivate()?;
        self.inner.committed.store(false, Ordering::Release);
        self.inner.stamp_on_drop.store(true, Ordering::Release);

        {
            let mut state = self.inner.gate.lock();
            state.committed = false;
            state.final_len = None;
            state.failed = None;
        }
        self.inner.gate.notify_all();
        Ok(())
    }

    /// Flush the final prefix without publishing readiness, then close the backing handles before
    /// the decorator renames and reopens the canonical file.
    pub(super) fn release_backing_inner(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.check_health()?;
        self.inner.driver.release_backing(final_len)
    }

    /// Whether dropping an uncommitted writer should mark the core failed.
    /// `false` once the resource is committed, already failed, cancelled
    /// (cancellation is a routine shutdown, not a writer error), or explicitly
    /// abandoned by a caller that owns the refill.
    pub(super) fn should_fail_on_drop(&self) -> bool {
        if self.inner.cancel.is_cancelled() || !self.inner.stamp_on_drop.load(Ordering::Acquire) {
            return false;
        }
        let state = self.inner.gate.lock();
        !state.committed && state.failed.is_none()
    }

    pub(super) fn status_inner(&self) -> ResourceStatus {
        let state = self.inner.gate.lock();
        if let Some(ref reason) = state.failed {
            ResourceStatus::Failed(reason.clone())
        } else if state.committed {
            ResourceStatus::Committed {
                final_len: state.final_len,
            }
        } else if self.inner.cancel.is_cancelled() {
            ResourceStatus::Cancelled
        } else {
            ResourceStatus::Active
        }
    }

    delegate::delegate! {
        to self.inner.driver {
            #[call(path)]
            pub(super) fn path_inner(&self) -> Option<&Path>;
        }
    }
}
