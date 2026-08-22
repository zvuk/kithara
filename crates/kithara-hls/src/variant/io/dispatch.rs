use kithara_assets::AssetsError;
use kithara_events::RequestPriority;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::StorageError;
use kithara_stream::dl::FetchCmd;
use kithara_test_utils::kithara;
use tracing::{debug, warn};

use super::{HlsVariant, PlanCtx, core::NO_PREFETCH_DEFERRAL};
use crate::segment::{Downloading, FetchClaim, PlannedFetch};

/// The cancel pair one dispatch rides. `fetch` covers owed work — inits,
/// size demands, and segments inside the owed window; `lookahead` covers an
/// audible session's prefetches past it, so a variant transition can retire
/// them without touching what playback waits on.
pub(crate) struct DispatchTokens {
    pub(crate) fetch: CancelToken,
    pub(crate) lookahead: CancelToken,
}

impl HlsVariant {
    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.flow.queue.lock().len() as u64
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
        ctx: &PlanCtx,
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
        let mut out = Vec::new();
        let mut deferred: Vec<PlannedFetch> = Vec::new();
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
        let prefetch_byte_cap = ctx
            .look_ahead_bytes
            .map(|n| prefetch_base.saturating_add(n));
        let prefetch_segment_cap = self.prefetch_segment_cap(ctx, prefetch_base);
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
            let Some(planned) = planned else { break };
            match planned {
                PlannedFetch::Init => {
                    let Some(init) = self.init() else {
                        continue;
                    };
                    let Some(handle) = init.state().try_claim(
                        PlannedFetch::Init,
                        Arc::downgrade(self),
                        ctx.signal.clone(),
                    ) else {
                        if !init.state().is_loaded() && !init.state().is_failed() {
                            deferred.push(planned);
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
                            deferred.push(planned);
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
                        Arc::downgrade(self),
                        ctx.signal.clone(),
                    ) else {
                        // WHY: An orphaned download may return to `Missing` and need another claim.
                        if !entry.state().is_loaded() && !entry.state().is_failed() {
                            deferred.push(planned);
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
                        deferred.push(planned);
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
            for planned in deferred.into_iter().rev() {
                // A concurrent claim's Drop may have requeued this entry
                // between the pop above and this write-back (a downloader
                // teardown racing the dispatch) — never double-plan it.
                if !queue.contains(&planned) {
                    queue.push_front(planned);
                }
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
        ctx: &PlanCtx,
        seg_idx: u32,
        handle: FetchClaim<Downloading>,
        cancel: CancelToken,
    ) -> Option<FetchCmd> {
        let entry = &self.segments[seg_idx as usize];
        let Some(resource_handle) = self.segment_handle(seg_idx) else {
            let _ = handle.into_missing();
            return None;
        };
        let resource = match resource_handle.acquire(entry.content()) {
            Ok(r) => r,
            Err(err) => {
                self.settle_unacquirable(seg_idx, handle, &err);
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

    /// Settle a claim whose resource could not be acquired.
    ///
    /// Reverting to `Missing` keeps the fetch on the plan, so `dispatch_from`
    /// tries again — right for a tmp a live sibling writer still holds, since
    /// that holder always settles and releases it. For anything else the retry
    /// never resolves, and the requeue is invisible: the slot stays planned, so
    /// `range_wait_phase` answers `WaitingDemand` and `range_has_failed` stays
    /// false while the decode gate parks for good. Those settle as `Failed`,
    /// which is what surfaces `SegmentUnavailable` to the reader.
    fn settle_unacquirable(
        &self,
        seg_idx: u32,
        handle: FetchClaim<Downloading>,
        err: &AssetsError,
    ) {
        if matches!(err, AssetsError::Storage(StorageError::TmpClaimed(_))) {
            debug!(
                variant = self.variant,
                seg_idx,
                error = %err,
                "emit_fetch_cmd: segment tmp held by a live writer; requeued"
            );
            let _ = handle.into_missing();
            return;
        }
        warn!(
            variant = self.variant,
            seg_idx,
            error = %err,
            "emit_fetch_cmd: segment resource cannot be acquired; settling as failed"
        );
        let _ = handle.into_failed();
    }

    fn prefetch_segment_cap(&self, ctx: &PlanCtx, prefetch_base: u64) -> Option<u32> {
        let window = look_ahead_segments(ctx)?;
        let base = self.descriptor_after_byte(prefetch_base)?.segment_index;
        Some(base.saturating_add(window.saturating_sub(1)))
    }

    fn segment_window_entry_byte(&self, ctx: &PlanCtx, seg_idx: u32) -> Option<u64> {
        let window = look_ahead_segments(ctx)?;
        self.segment_byte_offset(seg_idx.saturating_sub(window.saturating_sub(1)))
    }
}

fn look_ahead_segments(ctx: &PlanCtx) -> Option<u32> {
    let window = ctx.look_ahead_segments?;
    Some(u32::try_from(window.max(1)).unwrap_or(u32::MAX))
}
