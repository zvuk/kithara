use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use kithara_events::HlsEvent;
use kithara_net::NetError;
use kithara_platform::{Mutex, time::Instant};
use kithara_storage::ResourceExt;
use kithara_stream::dl::{FetchCmd, OnCompleteFn, Peer, Priority, WriterFn};
use tracing::debug;

use crate::{
    ids::SegmentId,
    loading::{SegmentLoader, segment_loader::PreparedMedia},
    scheduler::HlsScheduler,
};

/// All mutable state behind a single Mutex.
struct HlsState {
    scheduler: HlsScheduler,
    loader: Arc<SegmentLoader>,
    waker: Option<Waker>,
    epoch_cancel: tokio_util::sync::CancellationToken,
}

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After `activate()`: Downloader drives segment downloads via
/// self-contained `FetchCmd` closures (`writer` + `on_complete`).
pub(crate) struct HlsPeer {
    state: Arc<Mutex<Option<HlsState>>>,
    /// Waker stored before activation (`poll_next` called but state is None).
    pending_waker: Mutex<Option<Waker>>,
    /// Cancels the waker-forwarding micro-task on drop.
    wake_cancel: tokio_util::sync::CancellationToken,
}

impl HlsPeer {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            pending_waker: Mutex::new(None),
            wake_cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub(crate) fn activate(self: &Arc<Self>, scheduler: HlsScheduler, loader: Arc<SegmentLoader>) {
        let reader_advanced = scheduler.coord.reader_advanced.clone();
        let cancel = scheduler.coord.cancel.clone();

        {
            let mut guard = self.state.lock_sync();
            *guard = Some(HlsState {
                scheduler,
                loader,
                waker: None,
                epoch_cancel: tokio_util::sync::CancellationToken::new(),
            });
        }

        // Wake pending waker from pre-activation poll_next calls.
        if let Some(waker) = self.pending_waker.lock_sync().take() {
            waker.wake();
        }

        // Waker forwarding: translate Notify → stored waker.
        let peer = Arc::clone(self);
        let wake_cancel = self.wake_cancel.clone();
        kithara_platform::tokio::task::spawn(async move {
            loop {
                kithara_platform::tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = wake_cancel.cancelled() => return,
                    () = reader_advanced.notified() => {
                        let guard = peer.state.lock_sync();
                        if let Some(ref state) = *guard
                            && let Some(waker) = state.waker.as_ref()
                        {
                            waker.wake_by_ref();
                        }
                    }
                }
            }
        });
    }
}

impl Peer for HlsPeer {
    fn priority(&self) -> Priority {
        Priority::Low
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "HLS scheduler poll_next is inherently complex"
    )]
    #[expect(clippy::significant_drop_tightening)]
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let mut guard = self.state.lock_sync();
        let Some(ref mut state) = *guard else {
            *self.pending_waker.lock_sync() = Some(cx.waker().clone());
            return Poll::Pending;
        };

        state.waker = Some(cx.waker().clone());

        debug!(
            download_variant = state.scheduler.download_variant,
            cursor = state.scheduler.current_segment_index(),
            "poll_next: entry"
        );

        // 1. Demand processing.
        let (demand_segment, demand_variant_override) = match process_demand(state, cx) {
            DemandResult::ResetAndPend => return Poll::Pending,
            DemandResult::Demand {
                segment,
                variant_override,
            } => (Some(segment), variant_override),
            DemandResult::None => (None, None),
        };

        // 2. Flushing gate (prefetch only, demand bypasses above).
        if state.scheduler.coord.timeline().is_flushing() {
            debug!("poll_next: flushing, returning Pending");
            return Poll::Pending;
        }

        // 3. ABR decision + variant selection.
        let (old_variant, variant) = resolve_variant(&mut state.scheduler, demand_variant_override);

        let Some(num_segments) = state.scheduler.num_segments_for_plan(variant) else {
            debug!(variant, "poll_next: no num_segments, Pending");
            return Poll::Pending;
        };

        // 4. Tail check.
        if state.scheduler.handle_tail_state(variant, num_segments) {
            debug!(
                variant,
                num_segments,
                cursor = state.scheduler.current_segment_index(),
                reader_pos = state.scheduler.coord.timeline().byte_position(),
                eof = state.scheduler.coord.timeline().eof(),
                "poll_next: tail state, Pending"
            );
            return Poll::Pending;
        }

        // 5. Variant readiness (sync).
        apply_variant_readiness(
            &mut state.scheduler,
            variant,
            old_variant,
            demand_segment,
            demand_variant_override,
        );

        // 5b. Demand throttle.
        if let Some(ds) = demand_segment {
            let ahead = state
                .scheduler
                .look_ahead_segments
                .unwrap_or(state.scheduler.prefetch_count);
            state.scheduler.demand_throttle_until = Some(ds + ahead);
        }
        if demand_segment.is_none()
            && let Some(cap) = state.scheduler.demand_throttle_until
            && state.scheduler.current_segment_index() >= cap
        {
            return Poll::Pending;
        }

        // 6. Fill batch.
        let cmds = build_batch(
            state,
            &self.state,
            variant,
            old_variant,
            num_segments,
            demand_segment,
        );

        if cmds.is_empty() {
            if state.scheduler.current_segment_index() >= num_segments {
                if !state.scheduler.handle_tail_state(variant, num_segments) {
                    cx.waker().wake_by_ref();
                }
                return Poll::Pending;
            }
            debug!(
                variant,
                cursor = state.scheduler.current_segment_index(),
                num_segments,
                "poll_next: all cached, re-polling"
            );
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        debug!(
            variant,
            count = cmds.len(),
            cursor = state.scheduler.current_segment_index(),
            "poll_next: returning {} FetchCmds",
            cmds.len()
        );
        Poll::Ready(Some(cmds))
    }
}

// --- poll_next helper types and functions ---

enum DemandResult {
    ResetAndPend,
    Demand {
        segment: usize,
        variant_override: Option<usize>,
    },
    None,
}

fn process_demand(state: &mut HlsState, cx: &mut Context<'_>) -> DemandResult {
    let sched = &mut state.scheduler;

    let Some(req) = sched.next_valid_demand_request() else {
        return DemandResult::None;
    };

    if req.seek_epoch != sched.active_seek_epoch {
        // New seek epoch — full reset.
        state.epoch_cancel.cancel();
        state.epoch_cancel = tokio_util::sync::CancellationToken::new();
        sched.demand_throttle_until = None;

        let (is_variant_switch, is_midstream_switch) =
            sched.classify_variant_transition(req.variant, req.segment_index);
        sched.handle_midstream_switch(is_midstream_switch);
        if let Some(sizes) = sched.playlist_state.segment_sizes(req.variant) {
            sched
                .segments
                .lock_sync()
                .set_expected_sizes(req.variant, sizes);
        }
        if is_variant_switch {
            sched.download_variant = req.variant;
        }
        let (cached_count, cached_end_offset) =
            sched.populate_cached_segments_if_needed(req.variant);
        sched.apply_cached_segment_progress(req.variant, cached_count, cached_end_offset);

        cx.waker().wake_by_ref();
        return DemandResult::ResetAndPend;
    }

    // Same-epoch demand.
    let mut variant_override = None;
    if req.segment_index < sched.current_segment_index() {
        debug!(
            variant = req.variant,
            segment = req.segment_index,
            cursor = sched.current_segment_index(),
            "poll_next: same-epoch demand behind cursor, rewinding"
        );
        sched.rewind_current_segment_index(req.segment_index);
    }
    if req.variant != sched.download_variant {
        variant_override = Some(req.variant);
    }

    DemandResult::Demand {
        segment: req.segment_index,
        variant_override,
    }
}

fn resolve_variant(
    sched: &mut HlsScheduler,
    demand_variant_override: Option<usize>,
) -> (usize, usize) {
    let old_variant = sched.abr.get_current_variant_index();
    let decision = sched.make_abr_decision();
    let mut variant = sched.abr.get_current_variant_index();

    // Demand variant override.
    if let Some(dv) = demand_variant_override
        && dv != variant
    {
        debug!(
            demand_variant = dv,
            abr_variant = variant,
            "poll_next: demand override — filling layout gap"
        );
        variant = dv;
    }

    // Layout gap-fill override.
    if sched.filling_layout_gap
        && demand_variant_override.is_none()
        && sched.download_variant != variant
    {
        debug!(
            layout_variant = sched.download_variant,
            abr_variant = variant,
            "poll_next: continuing layout gap-fill"
        );
        variant = sched.download_variant;
    }

    sched.publish_variant_applied(old_variant, variant, &decision);

    (old_variant, variant)
}

fn apply_variant_readiness(
    sched: &mut HlsScheduler,
    variant: usize,
    _old_variant: usize,
    demand_segment: Option<usize>,
    demand_variant_override: Option<usize>,
) {
    let (is_variant_switch, is_midstream_switch) =
        sched.classify_variant_transition(variant, sched.current_segment_index());

    if demand_variant_override.is_none() {
        sched.handle_midstream_switch(is_midstream_switch);
    }
    if is_variant_switch {
        sched.download_variant = variant;
    }
    if let Some(sizes) = sched.playlist_state.segment_sizes(variant) {
        sched
            .segments
            .lock_sync()
            .set_expected_sizes(variant, sizes);
    }
    let (cached_count, cached_end_offset) = sched.populate_cached_segments_if_needed(variant);
    sched.apply_cached_segment_progress(variant, cached_count, cached_end_offset);

    // Demand cursor protection.
    if let Some(ds) = demand_segment
        && sched.current_segment_index() > ds
    {
        debug!(
            demand = ds,
            cursor = sched.current_segment_index(),
            "poll_next: demand cursor protection, resetting"
        );
        sched.reset_cursor(ds);
    }
}

fn build_batch(
    state: &mut HlsState,
    state_arc: &Arc<Mutex<Option<HlsState>>>,
    variant: usize,
    old_variant: usize,
    num_segments: usize,
    demand_segment: Option<usize>,
) -> Vec<FetchCmd> {
    let mut cmds = Vec::new();
    let seek_epoch = state.scheduler.coord.timeline().seek_epoch();
    let has_init = state.scheduler.variant_has_init(variant);
    let is_variant_switch = old_variant != variant;
    let (_, is_midstream_switch) = state
        .scheduler
        .classify_variant_transition(variant, state.scheduler.current_segment_index());
    let mut need_init = has_init
        && (state.scheduler.force_init_for_seek
            || state.scheduler.switch_needs_init(
                variant,
                state.scheduler.current_segment_index(),
                is_variant_switch,
            ));
    let prefetch_count = state.scheduler.prefetch_count;

    for batch_i in 0..prefetch_count {
        let seg_idx = state.scheduler.current_segment_index();
        if seg_idx >= num_segments {
            break;
        }

        let is_demanded = demand_segment == Some(seg_idx);
        let skipped = !is_demanded
            && state.scheduler.should_skip_planned_segment(
                variant,
                seg_idx,
                is_midstream_switch,
                if is_variant_switch {
                    Some(old_variant)
                } else {
                    None
                },
            );
        if skipped {
            debug!(
                variant,
                seg_idx,
                batch_i,
                cursor_after = state.scheduler.current_segment_index(),
                "poll_next: skipped segment"
            );
            continue;
        }

        state.scheduler.bus.publish(HlsEvent::SegmentStart {
            variant,
            segment_index: seg_idx,
            byte_offset: state.scheduler.coord.timeline().download_position(),
        });

        let plan_need_init = has_init
            && crate::scheduler::helpers::should_request_init(need_init, SegmentId::Media(seg_idx));
        if plan_need_init {
            state.scheduler.force_init_for_seek = false;
        }
        need_init = false;

        let prepared = match state.loader.prepare_media_sync(variant, seg_idx) {
            Ok(p) => p,
            Err(e) => {
                state
                    .scheduler
                    .publish_download_error("prepare_media_sync", &e);
                state.scheduler.advance_current_segment_index(seg_idx + 1);
                continue;
            }
        };

        // Cached hit — commit directly.
        if let Some(cached_len) = prepared.cached_len {
            let init_meta = if plan_need_init {
                state.loader.get_init_segment_cached(variant)
            } else {
                None
            };
            let init_len = init_meta.as_ref().map_or(0, |m| m.len);
            let init_url = init_meta.map(|m| m.url);

            let meta = match state.loader.complete_media(&prepared, cached_len) {
                Ok(m) => m,
                Err(e) => {
                    state
                        .scheduler
                        .publish_download_error("complete_media cached", &e);
                    state.scheduler.advance_current_segment_index(seg_idx + 1);
                    continue;
                }
            };

            let cursor_before = state.scheduler.current_segment_index();
            state.scheduler.commit_fetch_inline(
                variant,
                seg_idx,
                seek_epoch,
                &meta,
                init_len,
                init_url,
                std::time::Duration::ZERO,
            );
            state.scheduler.advance_current_segment_index(seg_idx + 1);
            debug!(
                variant,
                seg_idx,
                batch_i,
                cached_len,
                cursor_before,
                cursor_after = state.scheduler.current_segment_index(),
                "poll_next: cached commit"
            );
            continue;
        }

        // Build self-contained FetchCmd for network download.
        let cmd = build_fetch_cmd(
            state,
            state_arc,
            variant,
            seg_idx,
            seek_epoch,
            prepared,
            plan_need_init,
        );
        cmds.push(cmd);

        state.scheduler.advance_current_segment_index(seg_idx + 1);

        if is_demanded {
            break;
        }
    }

    cmds
}

#[expect(clippy::significant_drop_tightening)]
fn build_fetch_cmd(
    state: &HlsState,
    state_arc: &Arc<Mutex<Option<HlsState>>>,
    variant: usize,
    seg_idx: usize,
    seek_epoch: u64,
    prepared: PreparedMedia,
    plan_need_init: bool,
) -> FetchCmd {
    let resource = prepared
        .resource
        .clone()
        .expect("non-cached PreparedMedia must have resource");

    let init_meta = if plan_need_init {
        state.loader.get_init_segment_cached(variant)
    } else {
        None
    };
    let init_len = init_meta.as_ref().map_or(0, |m| m.len);
    let init_url = init_meta.map(|m| m.url);
    let url = prepared.url.clone();

    // Writer closure.
    let offset = Arc::new(AtomicU64::new(0));
    let res_w = resource.clone();
    let off_w = Arc::clone(&offset);
    let writer: WriterFn = Box::new(move |data: &[u8]| {
        let pos = off_w.fetch_add(data.len() as u64, Ordering::Relaxed);
        res_w.write_at(pos, data).map_err(io::Error::other)
    });

    // on_complete closure.
    let state_arc = Arc::clone(state_arc);
    let start = Instant::now();
    let on_complete: OnCompleteFn =
        Box::new(move |bytes_written: u64, error: Option<&NetError>| {
            if let Some(e) = error {
                let is_already_committed =
                    e.to_string().contains("cannot write to committed resource");
                let mut guard = state_arc.lock_sync();
                let Some(ref mut st) = *guard else {
                    return;
                };
                if !is_already_committed {
                    debug!(variant, seg_idx, error = %e, "segment fetch failed, rewinding cursor");
                    st.scheduler.rewind_current_segment_index(seg_idx);
                }
                if let Some(waker) = st.waker.as_ref() {
                    waker.wake_by_ref();
                }
                return;
            }
            let loader = {
                let guard = state_arc.lock_sync();
                let Some(ref st) = *guard else {
                    return;
                };
                Arc::clone(&st.loader)
            };
            let meta = loader.complete_media(&prepared, bytes_written);
            let mut guard = state_arc.lock_sync();
            let Some(ref mut st) = *guard else {
                return;
            };
            if let Ok(ref meta) = meta {
                st.scheduler.commit_fetch_inline(
                    variant,
                    seg_idx,
                    seek_epoch,
                    meta,
                    init_len,
                    init_url.clone(),
                    start.elapsed(),
                );
            }
            if let Some(waker) = st.waker.as_ref() {
                waker.wake_by_ref();
            }
        });

    FetchCmd::get(url)
        .cancel(Some(state.epoch_cancel.clone()))
        .writer(writer)
        .on_complete(on_complete)
}
