use std::sync::Arc;

use kithara_stream::dl::FetchCmd;
use kithara_test_utils::kithara;
use tracing::debug;

use super::{HlsVariant, PlanCtx};
use crate::segment::{Downloading, FetchClaim, PlannedFetch};

impl HlsVariant {
    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.flow.queue.lock().len() as u64
    )]
    #[kithara::hang_watchdog]
    pub(crate) fn dispatch(self: &Arc<Self>, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        // Popped segments that could not be dispatched this pass but are NOT
        // terminal (a slot still `Downloading` under an orphaned/in-flight
        // fetch, or one that raced back to `Missing`): re-queued at the front
        // after the pass so a later dispatch re-claims them once the slot
        // frees. Without this, a seek that re-queues the target while an old
        // prefetch still holds it `Downloading` would pop+drop the target —
        // the orphaned fetch settles back to `Missing` but the queue no longer
        // references it, so it is never re-fetched and the reader hangs (the
        // `player_worker_hls_then_unavailable_mp3_then_mp3_recovery` deadlock).
        let mut deferred: Vec<PlannedFetch> = Vec::new();
        let mut remaining = budget;
        self.dispatch_size_demands(ctx, &mut out, &mut remaining);
        let prefetch_base = self.get_position().max(self.prefetch_anchor());
        let prefetch_byte_cap = ctx
            .look_ahead_bytes
            .map(|n| prefetch_base.saturating_add(n));
        let prefetch_segment_cap = self.prefetch_segment_cap(ctx, prefetch_base);
        while remaining > 0 {
            hang_tick!();
            let planned = {
                let mut queue = self.flow.queue.lock();
                match queue.front().copied() {
                    None => break,
                    Some(PlannedFetch::Init) => queue.pop_front(),
                    Some(PlannedFetch::Segment(seg_idx)) => {
                        if let Some(cap) = prefetch_byte_cap
                            && let Some(seg_off) = self.segment_byte_offset(seg_idx)
                            && seg_off > cap
                        {
                            break;
                        }
                        if let Some(cap) = prefetch_segment_cap
                            && seg_idx > cap
                        {
                            break;
                        }
                        queue.pop_front()
                    }
                }
            };
            let Some(planned) = planned else { break };
            match planned {
                PlannedFetch::Init => {
                    // Only a present init is ever enqueued (the `rebuild`
                    // gate skips a `None` init), so a missing slot here is
                    // unreachable; skip defensively rather than claim a slot
                    let Some(init) = self.init() else {
                        continue;
                    };
                    let Some(handle) = init
                        .state()
                        .try_claim(PlannedFetch::Init, Arc::downgrade(self))
                    else {
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
                    let Some(cmd) = self.build_init_cmd(ctx, handle) else {
                        if self
                            .init()
                            .is_some_and(|i| !i.state().is_loaded() && !i.state().is_failed())
                        {
                            deferred.push(planned);
                        }
                        continue;
                    };
                    out.push(cmd);
                }
                PlannedFetch::Segment(seg_idx) => {
                    let Some(entry) = self.segments.get(seg_idx as usize) else {
                        continue;
                    };
                    let Some(handle) = entry
                        .state()
                        .try_claim(PlannedFetch::Segment(seg_idx), Arc::downgrade(self))
                    else {
                        // Claim failed — another claim owns the slot. Re-queue
                        // unless terminal (`Loaded` = already fetched, `Failed`
                        // = gave up): a `Downloading` orphan settles back to
                        // `Missing` and must stay re-claimable from the queue.
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
                    let Some(cmd) = self.emit_fetch_cmd(ctx, seg_idx, handle) else {
                        // Acquire raced (the claim was reverted to `Missing`
                        // inside `emit_fetch_cmd`): re-queue, don't drop it.
                        deferred.push(planned);
                        continue;
                    };
                    out.push(cmd);
                }
            }
            remaining -= 1;
        }
        if !deferred.is_empty() {
            let mut queue = self.flow.queue.lock();
            for planned in deferred.into_iter().rev() {
                queue.push_front(planned);
            }
        }
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
    ) -> Option<FetchCmd> {
        let entry = &self.segments[seg_idx as usize];
        let Some(resource_handle) = self.segment_handle(seg_idx) else {
            let _ = handle.into_missing();
            return None;
        };
        let resource = match resource_handle.acquire(entry.content()) {
            Ok(r) => r,
            Err(err) => {
                debug!(
                    variant = self.variant,
                    seg_idx,
                    error = %err,
                    "emit_fetch_cmd: acquire_resource dropped (variant switch in flight)"
                );
                let _ = handle.into_missing();
                return None;
            }
        };
        self.build_cmd(
            resource_handle.url().clone(),
            resource,
            handle,
            ctx.signal.clone(),
        )
    }

    fn prefetch_segment_cap(&self, ctx: &PlanCtx, prefetch_base: u64) -> Option<u32> {
        let window = ctx.look_ahead_segments?;
        let window = u32::try_from(window.max(1)).unwrap_or(u32::MAX);
        let base = self.descriptor_after_byte(prefetch_base)?.segment_index;
        Some(base.saturating_add(window.saturating_sub(1)))
    }
}
