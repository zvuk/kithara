use kithara_assets::AssetsError;
use kithara_bufpool::HasPool;
use kithara_events::RequestPriority;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::StorageError;
use kithara_stream::dl::FetchCmd;
use kithara_test_utils::kithara;
use tracing::{debug, warn};

use super::{HlsVariant, PlanCtx, PlanRevision, core::NO_PREFETCH_DEFERRAL};
use crate::segment::{Downloading, FetchClaim, PlannedFetch, Segment};

/// The cancel pair one dispatch rides. `fetch` covers owed work — inits,
/// size demands, and segments inside the owed window; `lookahead` covers an
/// audible session's prefetches past it, so a variant transition can retire
/// them without touching what playback waits on.
pub(crate) struct DispatchTokens {
    pub(crate) fetch: CancelToken,
    pub(crate) lookahead: CancelToken,
}

/// What a dispatch-side acquire failure does to the slot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AcquireSettle {
    /// Back onto the plan, so `dispatch_from` tries this segment again.
    Requeue,
    /// Terminal, which is what surfaces `SegmentUnavailable` to the reader.
    Fail,
}

/// Decide how an acquire failure settles, given how many the slot has already
/// taken (this one included).
///
/// A live sibling writer holding the tmp is the one obstruction the system
/// itself promises to clear: that holder always settles and releases, so the
/// retry is guaranteed to resolve and needs no budget. Nothing promises as
/// much for any other error — but a momentary one, a descriptor the host was
/// briefly short of or a parent directory a concurrent eviction was removing,
/// must not cost the whole segment either. Those retry against `budget`
/// ([`crate::HlsConfig::acquire_attempt_budget`]) and settle `Fail` once it is spent,
/// because a requeue that never resolves is invisible: the slot stays planned,
/// so `range_wait_phase` answers `WaitingDemand` and `range_has_failed` stays
/// false while the decode gate parks for good.
const fn settle_for(err: &AssetsError, failures: u8, budget: u8) -> AcquireSettle {
    if matches!(err, AssetsError::Storage(StorageError::TmpClaimed(_))) {
        return AcquireSettle::Requeue;
    }
    if failures < budget {
        AcquireSettle::Requeue
    } else {
        AcquireSettle::Fail
    }
}

impl<S> HlsVariant<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.flow.queue.lock().len() as u64,
        queue_head = self.flow.queue.lock().front().map_or(u64::MAX, |f| match f {
            PlannedFetch::Init => u64::MAX - 1,
            PlannedFetch::Segment(idx) => u64::from(*idx),
        }),
        cap = construction_segment_end.map_or(u64::MAX, u64::from)
    )]
    /// Emits prioritized fetches within construction and look-ahead bounds.
    ///
    /// Owed fetches ride `tokens.fetch`; an audible session's prefetches past
    /// the owed window (the playing segment and the next) ride
    /// `tokens.lookahead`, which a variant transition retires to free
    /// downloader capacity without touching what playback waits on.
    #[kithara::hang_watchdog]
    pub(crate) fn dispatch_from(
        self: &Arc<Self>,
        ctx: &PlanCtx<S>,
        budget: usize,
        position: u64,
        construction_segment_end: Option<u32>,
        audible: bool,
        tokens: DispatchTokens,
    ) -> Vec<FetchCmd> {
        let DispatchTokens {
            fetch: cancel,
            lookahead,
        } = tokens;
        let mut out: Vec<FetchCmd> = Vec::new();
        let mut deferred: Vec<(PlannedFetch, PlanRevision)> = Vec::new();
        let owed_through = audible
            .then(|| self.find_at_offset(position).map(|(seg_idx, _, _)| seg_idx))
            .flatten();
        let owed = |seg_idx: u32| !audible || owed_through.is_some_and(|last| seg_idx <= last);
        let beyond_owed = |seg_idx: u32| {
            audible && owed_through.is_some_and(|last| seg_idx > last.saturating_add(1))
        };
        let mut remaining = budget;
        self.dispatch_size_demands(ctx, &mut out, &mut remaining, &cancel);
        let prefetch_base = position.max(self.prefetch_anchor());
        // A construction bound names a debt: everything up to it must land for
        // the splice or build to finish. Look-ahead trims optional prefetch
        // only, so inside a bounded window the caps step aside.
        let prefetch_byte_cap = construction_segment_end
            .is_none()
            .then(|| {
                ctx.config
                    .look_ahead_bytes
                    .map(|n| prefetch_base.saturating_add(n))
            })
            .flatten();
        let prefetch_segment_cap = construction_segment_end
            .is_none()
            .then(|| self.prefetch_segment_cap(ctx, prefetch_base))
            .flatten();
        let mut resume_at: Option<u64> = None;
        while remaining > 0 {
            hang_tick!();
            let planned = {
                let mut queue = self.flow.queue.lock();
                match queue.front().copied() {
                    None => break,
                    Some(PlannedFetch::Init) => queue.pop_front(),
                    Some(PlannedFetch::Segment(seg_idx)) => {
                        if construction_segment_end.is_some_and(|end| seg_idx > end) {
                            break;
                        }
                        if let Some(cap) = prefetch_byte_cap
                            && let Some(seg_off) = self.segment_byte_offset(seg_idx)
                            && seg_off > cap
                        {
                            resume_at = ctx
                                .config
                                .look_ahead_bytes
                                .map(|window| seg_off.saturating_sub(window));
                            break;
                        }
                        if let Some(cap) = prefetch_segment_cap
                            && seg_idx > cap
                        {
                            resume_at = self.segment_window_entry_byte(ctx, seg_idx);
                            break;
                        }
                        queue.pop_front()
                    }
                }
            };
            let Some((planned, plan_revision)) = planned else {
                break;
            };
            match planned {
                PlannedFetch::Init => {
                    let Some(init) = self.init() else {
                        continue;
                    };
                    let Some(handle) = init.state().try_claim(
                        PlannedFetch::Init,
                        plan_revision,
                        Arc::downgrade(self),
                        ctx.signal.clone(),
                    ) else {
                        if !init.state().is_loaded() && !init.state().is_failed() {
                            deferred.push((planned, plan_revision));
                        }
                        continue;
                    };
                    if let Some(actual) = self.init_committed_final_len() {
                        handle.into_loaded(actual);
                        ctx.signal.fire();
                        continue;
                    }
                    let Some(mut cmd) = self.build_init_cmd(ctx, handle, cancel.clone()) else {
                        if self
                            .init()
                            .is_some_and(|i| !i.state().is_loaded() && !i.state().is_failed())
                        {
                            deferred.push((planned, plan_revision));
                        }
                        continue;
                    };
                    // WHY: A decoder cannot start until its init fetch completes.
                    cmd.set_priority(RequestPriority::High);
                    out.push(cmd);
                }
                PlannedFetch::Segment(seg_idx) => {
                    let Some(entry) = self.segments.get(seg_idx as usize) else {
                        continue;
                    };
                    let Some(handle) = entry.state().try_claim(
                        PlannedFetch::Segment(seg_idx),
                        plan_revision,
                        Arc::downgrade(self),
                        ctx.signal.clone(),
                    ) else {
                        // WHY: An orphaned download may return to `Missing` and need another claim.
                        if !entry.state().is_loaded() && !entry.state().is_failed() {
                            deferred.push((planned, plan_revision));
                        }
                        continue;
                    };
                    if let Some(actual) = self.committed_final_len(seg_idx) {
                        handle.into_loaded(actual);
                        ctx.signal.fire();
                        continue;
                    }
                    let token = if beyond_owed(seg_idx) {
                        lookahead.clone()
                    } else {
                        cancel.clone()
                    };
                    let Some(mut cmd) = self.emit_fetch_cmd(ctx, seg_idx, handle, token) else {
                        // WHY: A reverted claim must remain queued for another acquisition attempt.
                        deferred.push((planned, plan_revision));
                        continue;
                    };
                    if owed(seg_idx) {
                        cmd.set_priority(RequestPriority::High);
                    }
                    out.push(cmd);
                }
            }
            remaining -= 1;
        }
        if !deferred.is_empty() {
            let mut queue = self.flow.queue.lock();
            for (planned, revision) in deferred {
                // A concurrent claim's Drop may have requeued this entry
                // between the pop above and this write-back (a downloader
                // teardown racing the dispatch) — never double-plan it.
                queue.requeue_if_current(planned, revision);
            }
        }
        self.defer_prefetch_until(resume_at.unwrap_or(NO_PREFETCH_DEFERRAL));
        out
    }

    #[kithara::probe(
        seek_epoch = ctx.seek_epoch,
        segment_index = u64::from(seg_idx),
        variant = self.variant as u64
    )]
    fn emit_fetch_cmd(
        self: &Arc<Self>,
        ctx: &PlanCtx<S>,
        seg_idx: u32,
        handle: FetchClaim<Downloading, S>,
        cancel: CancelToken,
    ) -> Option<FetchCmd> {
        let entry = &self.segments[seg_idx as usize];
        let Some(resource_handle) = self.segment_handle(seg_idx) else {
            let _ = handle.into_missing();
            return None;
        };
        let resource = match resource_handle.acquire(entry.content()) {
            Ok(r) => {
                entry.state().clear_acquire_failures();
                r
            }
            Err(err) => {
                self.settle_unacquirable(ctx, entry, seg_idx, handle, &err);
                return None;
            }
        };
        self.build_cmd(
            resource_handle.url().clone(),
            resource,
            handle,
            ctx.signal.clone(),
            cancel,
        )
    }

    /// Settle a claim whose resource could not be acquired, per
    /// [`settle_for`].
    fn settle_unacquirable(
        &self,
        ctx: &PlanCtx<S>,
        entry: &Segment,
        seg_idx: u32,
        handle: FetchClaim<Downloading, S>,
        err: &AssetsError,
    ) {
        let failures = entry.state().note_acquire_failure();
        match settle_for(err, failures, ctx.config.acquire_attempt_budget) {
            AcquireSettle::Requeue => {
                debug!(
                    variant = self.variant,
                    seg_idx,
                    failures,
                    error = %err,
                    "emit_fetch_cmd: segment resource not acquirable yet; requeued"
                );
                let _ = handle.into_missing();
            }
            AcquireSettle::Fail => {
                warn!(
                    variant = self.variant,
                    seg_idx,
                    failures,
                    error = %err,
                    "emit_fetch_cmd: segment resource cannot be acquired; settling as failed"
                );
                let _ = handle.into_failed();
            }
        }
    }

    fn prefetch_segment_cap(&self, ctx: &PlanCtx<S>, prefetch_base: u64) -> Option<u32> {
        let window = look_ahead_segments(ctx)?;
        let base = self.descriptor_after_byte(prefetch_base)?.segment_index;
        Some(base.saturating_add(window.saturating_sub(1)))
    }

    fn segment_window_entry_byte(&self, ctx: &PlanCtx<S>, seg_idx: u32) -> Option<u64> {
        let window = look_ahead_segments(ctx)?;
        self.segment_byte_offset(seg_idx.saturating_sub(window.saturating_sub(1)))
    }
}

fn look_ahead_segments<S>(ctx: &PlanCtx<S>) -> Option<u32>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let window = ctx.config.look_ahead_segments?;
    Some(u32::try_from(window.max(1)).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use std::{io, path::PathBuf};

    use super::*;
    use crate::config::DEFAULT_ACQUIRE_ATTEMPT_BUDGET;

    fn tmp_claimed() -> AssetsError {
        AssetsError::Storage(StorageError::TmpClaimed(PathBuf::from("seg.m4s.tmp")))
    }

    fn other() -> AssetsError {
        AssetsError::Io(io::Error::from(io::ErrorKind::IsADirectory))
    }

    const BUDGET: u8 = DEFAULT_ACQUIRE_ATTEMPT_BUDGET;

    /// The holder of a claimed tmp always settles and releases it, so this
    /// retry resolves on its own however long it takes.
    #[kithara::test]
    fn a_claimed_tmp_requeues_without_a_budget() {
        for failures in [1, BUDGET, u8::MAX] {
            assert_eq!(
                settle_for(&tmp_claimed(), failures, BUDGET),
                AcquireSettle::Requeue,
                "a live writer's tmp must never fail the slot ({failures} failures)"
            );
        }
    }

    /// Every other error gets retried, because the momentary ones are
    /// indistinguishable from the standing ones at the point of failure.
    #[kithara::test]
    fn another_error_retries_inside_the_budget() {
        for failures in 1..BUDGET {
            assert_eq!(
                settle_for(&other(), failures, BUDGET),
                AcquireSettle::Requeue,
                "failure {failures} of {BUDGET} must not be terminal"
            );
        }
    }

    /// And is given up on once the budget is spent, so a standing obstruction
    /// reaches the reader instead of parking the decode gate for good.
    #[kithara::test]
    fn another_error_fails_the_slot_once_the_budget_is_spent() {
        assert_eq!(settle_for(&other(), BUDGET, BUDGET), AcquireSettle::Fail);
    }

    /// Saturating the counter must not wrap back under the budget and hand a
    /// standing obstruction a fresh set of retries.
    #[kithara::test]
    fn a_saturated_counter_stays_terminal() {
        assert_eq!(settle_for(&other(), u8::MAX, BUDGET), AcquireSettle::Fail);
    }

    /// The budget is a config field, not a constant: a caller that raises it
    /// gets the extra rounds, and one that zeroes it settles on the first
    /// failure.
    #[kithara::test]
    fn the_budget_comes_from_the_caller() {
        assert_eq!(
            settle_for(&other(), BUDGET, BUDGET + 1),
            AcquireSettle::Requeue,
            "a raised budget must grant the extra round"
        );
    }

    #[kithara::test]
    fn a_zero_budget_settles_on_the_first_failure() {
        assert_eq!(settle_for(&other(), 1, 0), AcquireSettle::Fail);
    }
}
