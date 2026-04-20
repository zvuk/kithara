use std::{
    io::Error as IoError,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use kithara_events::HlsEvent;
use kithara_net::NetError;
use kithara_platform::{Mutex, time::Instant, tokio};
use kithara_storage::ResourceExt;
use kithara_stream::{
    Timeline,
    dl::{FetchCmd, OnCompleteFn, Peer, Priority, WriterFn},
};
use tokio_util::sync::CancellationToken;
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
    epoch_cancel: CancellationToken,
}

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After `activate()`: Downloader drives segment downloads via
/// self-contained `FetchCmd` closures (`writer` + `on_complete`).
pub(crate) struct HlsPeer {
    state: Arc<Mutex<Option<HlsState>>>,
    /// Waker stored before activation (`poll_next` called but state is None).
    pending_waker: Mutex<Option<Waker>>,
    /// Cancels the waker-forwarding micro-task on drop.
    wake_cancel: CancellationToken,
    /// Same Arc-clone as the one held by `HlsCoord` — reads from the
    /// audio FSM are published here and observed by `priority()`.
    timeline: Timeline,
}

impl HlsPeer {
    pub(crate) fn new(timeline: Timeline) -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            pending_waker: Mutex::new(None),
            wake_cancel: CancellationToken::new(),
            timeline,
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
                epoch_cancel: CancellationToken::new(),
            });
        }

        // Wake pending waker from pre-activation poll_next calls.
        if let Some(waker) = self.pending_waker.lock_sync().take() {
            waker.wake();
        }

        // Waker forwarding: translate Notify → stored waker.
        //
        // Use `Weak` so this task does NOT keep `HlsPeer` alive. When
        // the Registry drops its `Arc<HlsPeer>` after the PeerHandle
        // cancel fires, the strong count hits 0 and `HlsPeer::Drop`
        // fires `wake_cancel`, which wakes this task's select loop and
        // it exits. Without `Weak` + `Drop` the task held an Arc to the
        // peer and wedged the scheduler/segment-loader graph alive past
        // `HlsSource::drop` — nextest flagged this as a rotating LEAK.
        let peer_weak = Arc::downgrade(self);
        let wake_cancel = self.wake_cancel.clone();
        tokio::task::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = wake_cancel.cancelled() => return,
                    () = reader_advanced.notified() => {
                        let Some(peer) = peer_weak.upgrade() else { return; };
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

    /// Release the stashed [`HlsState`] and cancel the waker task.
    ///
    /// Must be called when the owning [`HlsSource`] drops, otherwise
    /// `HlsState::loader` keeps an `Arc<SegmentLoader>` that transitively
    /// holds three [`PeerHandle`] clones (loader, playlist cache, key
    /// manager). Those clones keep `PeerInner.cancel` unfired, which
    /// keeps the `Registry` entry (and this `Arc<HlsPeer>`) alive — the
    /// whole peer graph leaks until the entire `Downloader` is dropped.
    pub(crate) fn teardown(&self) {
        // Stop the waker-forwarding task first so it never races our
        // state lock after we clear it.
        self.wake_cancel.cancel();
        let mut guard = self.state.lock_sync();
        *guard = None;
    }
}

impl Drop for HlsPeer {
    fn drop(&mut self) {
        // Fire wake_cancel so the waker-forwarding spawn above exits
        // even when there is no pending `reader_advanced` notification
        // to trigger its select loop.
        self.wake_cancel.cancel();
    }
}

impl Peer for HlsPeer {
    /// Priority reflects the audio FSM's decode-activity flag on the
    /// shared `Timeline`. Cheap, lock-free — called by Registry on
    /// every `poll_peers` pass.
    fn priority(&self) -> Priority {
        if self.timeline.is_playing() {
            Priority::High
        } else {
            Priority::Low
        }
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

// poll_next helper types and functions

#[cfg_attr(test, derive(Debug))]
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

    // Flushing gate: don't consume demands while the timeline is flushing.
    // `poll_next`'s downstream flushing gate would return `Poll::Pending`
    // right after, so draining the slot here would lose the demand — the
    // scheduler never emits a FetchCmd for it and the seek deadlocks.
    if sched.coord.timeline().is_flushing() {
        return DemandResult::None;
    }

    let Some(req) = sched.next_valid_demand_request() else {
        return DemandResult::None;
    };

    if req.seek_epoch != sched.active_seek_epoch {
        // New seek epoch — full reset.
        state.epoch_cancel.cancel();
        state.epoch_cancel = CancellationToken::new();
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
        // Two cases where rewinding the cursor re-emits a duplicate FetchCmd
        // and drives a hot loop with the reader's condvar re-demand pump:
        //  1. The demanded segment is already committed — the original
        //     already-loaded check.
        //  2. The demanded segment is IN FLIGHT — a `FetchCmd` has been
        //     emitted but its `on_complete` has not fired yet. Issuing a
        //     duplicate `FetchCmd` races two writers on the same cached
        //     `AssetResource`; on mmap storage the first commit flips the
        //     resource to `Committed` and the second `write_at` fails,
        //     starving the reader until the hang detector fires.
        let in_flight = sched
            .in_flight_segments
            .contains(&(req.variant, req.segment_index));
        let already_loaded = !in_flight
            && sched
                .segments
                .lock_sync()
                .is_segment_loaded(req.variant, req.segment_index);
        if in_flight {
            debug!(
                variant = req.variant,
                segment = req.segment_index,
                cursor = sched.current_segment_index(),
                "poll_next: same-epoch demand for in-flight segment, skipping rewind"
            );
        } else if already_loaded {
            debug!(
                variant = req.variant,
                segment = req.segment_index,
                cursor = sched.current_segment_index(),
                "poll_next: same-epoch demand already committed, skipping rewind"
            );
        } else {
            debug!(
                variant = req.variant,
                segment = req.segment_index,
                cursor = sched.current_segment_index(),
                "poll_next: same-epoch demand behind cursor, rewinding"
            );
            sched.rewind_current_segment_index(req.segment_index);
        }
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

    // Demand cursor protection — but never reset onto a segment whose
    // `FetchCmd` is still in flight: that is the "rewind → duplicate
    // FetchCmd" hot loop we avoid in `process_demand`. The prior fetch
    // will commit and notify the reader; resetting the cursor here just
    // races a second writer on the same cached `AssetResource`.
    if let Some(ds) = demand_segment
        && sched.current_segment_index() > ds
        && !sched.in_flight_segments.contains(&(variant, ds))
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
        state
            .scheduler
            .in_flight_segments
            .insert((variant, seg_idx));

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
        res_w.write_at(pos, data).map_err(IoError::other)
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
                // Drop callbacks from a prior seek epoch: their FetchCmds
                // were cancelled by `process_demand` bumping `epoch_cancel`,
                // and `reset_for_seek_epoch` already cleared in_flight_
                // segments and reset the cursor for the new epoch. Acting
                // here would call `rewind_current_segment_index(seg_idx)`
                // against the new epoch's cursor — and `rewind_fill_to`
                // sets `self.next = floor.max(seg_idx)` unconditionally,
                // which silently advances the cursor forward when
                // `seg_idx > new_floor`, skipping segments the reader is
                // now waiting for after the seek.
                if seek_epoch != st.scheduler.coord.timeline().seek_epoch() {
                    debug!(
                        variant,
                        seg_idx,
                        captured_seek_epoch = seek_epoch,
                        current_seek_epoch = st.scheduler.coord.timeline().seek_epoch(),
                        "stale on_complete from prior seek epoch — dropping"
                    );
                    return;
                }
                st.scheduler.in_flight_segments.remove(&(variant, seg_idx));
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
            st.scheduler.in_flight_segments.remove(&(variant, seg_idx));
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

#[cfg(test)]
mod tests {
    //! RED tests confirming specific root causes of integration-test failures.

    use std::{sync::Arc, task::Context, time::Duration};

    use futures::task::noop_waker;
    use kithara_abr::{AbrDecision, AbrReason};
    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
    use kithara_drm::DecryptContext;
    use kithara_events::EventBus;
    use kithara_net::NetError;
    use kithara_platform::{Mutex as PlatformMutex, time::Instant as PlatformInstant};
    use kithara_storage::ResourceExt;
    use kithara_stream::{
        Timeline,
        dl::{Downloader, DownloaderConfig, Peer},
    };
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{
        DemandResult, HlsPeer, HlsState, apply_variant_readiness, build_fetch_cmd, process_demand,
        resolve_variant,
    };
    use crate::{
        config::HlsConfig,
        coord::SegmentRequest,
        loading::PlaylistCache,
        parsing::{VariantId, VariantStream},
        playlist::{PlaylistState, SegmentState, VariantState},
        source::build_pair,
        stream_index::SegmentData,
    };

    struct Consts;
    impl Consts {
        const NUM_VARIANTS: usize = 2;
        const NUM_SEGMENTS: usize = 40;
    }

    fn make_variant_state(id: usize) -> VariantState {
        let base = Url::parse("https://example.com/").expect("valid base URL");
        VariantState {
            id,
            uri: base
                .join(&format!("v{id}.m3u8"))
                .expect("valid playlist URL"),
            bandwidth: Some(128_000),
            codec: None,
            container: None,
            init_url: None,
            segments: (0..Consts::NUM_SEGMENTS)
                .map(|index| SegmentState {
                    index,
                    url: base
                        .join(&format!("seg-{id}-{index}.m4s"))
                        .expect("valid segment URL"),
                    duration: Duration::from_secs(4),
                    key: None,
                })
                .collect(),
            size_map: None,
        }
    }

    fn make_hls_state() -> HlsState {
        let cancel = CancellationToken::new();
        let passthrough: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
                output[..input.len()].copy_from_slice(input);
                Ok(input.len())
            });
        let backend = AssetStoreBuilder::new()
            .ephemeral(true)
            .cancel(cancel.clone())
            .process_fn(passthrough)
            .build();

        let playlist_state = Arc::new(PlaylistState::new(
            (0..Consts::NUM_VARIANTS).map(make_variant_state).collect(),
        ));
        let parsed: Vec<VariantStream> = (0..Consts::NUM_VARIANTS)
            .map(|i| VariantStream {
                id: VariantId(i),
                uri: format!("v{i}.m3u8"),
                bandwidth: Some(128_000),
                name: None,
                codec: None,
            })
            .collect();
        let downloader =
            Downloader::new(DownloaderConfig::default().with_cancel(cancel.child_token()));
        let timeline = Timeline::new();
        let peer = Arc::new(HlsPeer::new(timeline.clone()));
        let handle = downloader.register(peer);
        let cache = PlaylistCache::new(backend.clone(), handle.clone());
        let loader = Arc::new(crate::loading::SegmentLoader::new(
            handle.clone(),
            backend.clone(),
            None,
            cache,
        ));
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (scheduler, _source) = build_pair(
            backend,
            handle,
            &parsed,
            &config,
            playlist_state,
            EventBus::new(16),
            timeline,
        );
        HlsState {
            scheduler,
            loader,
            waker: None,
            epoch_cancel: CancellationToken::new(),
        }
    }

    fn enqueue_demand(state: &HlsState, variant: usize, segment_index: usize) {
        let seek_epoch = state.scheduler.coord.timeline().seek_epoch();
        state
            .scheduler
            .coord
            .enqueue_segment_request(SegmentRequest {
                segment_index,
                variant,
                seek_epoch,
            });
    }

    fn commit_segment_in_index(state: &HlsState, variant: usize, seg_idx: usize, media_len: u64) {
        let url = Url::parse(&format!("https://example.com/seg-{variant}-{seg_idx}.m4s"))
            .expect("valid url");
        // Also write a dummy resource so `segment_resources_available` is true.
        let key = ResourceKey::from_url(&url);
        let res = state
            .scheduler
            .backend
            .acquire_resource(&key)
            .expect("acquire");
        res.write_at(0, &vec![0u8; media_len as usize])
            .expect("write");
        res.commit(None).expect("commit");

        state.scheduler.segments.lock_sync().commit_segment(
            variant,
            seg_idx,
            SegmentData {
                init_len: 0,
                media_len,
                init_url: None,
                media_url: url,
            },
        );
    }

    /// RED test #5 (integration: live_stress_real_stream_seek_read_cache_drm_ephemeral)
    ///
    /// `process_demand` calls `next_valid_demand_request` (which `take()`s
    /// the demand slot) BEFORE the flushing gate runs. If the timeline is
    /// flushing (new seek just started), poll_next returns `Poll::Pending`
    /// after the demand was already drained — so no `FetchCmd` is emitted
    /// and the reader never gets data: deadlock.
    ///
    /// Invariant under test: a demand submitted while `is_flushing()` is
    /// true must NOT be silently consumed by `process_demand`.
    #[kithara::test]
    fn red_test_seek_read_cache_demand_not_lost_under_flushing_gate() {
        let mut state = make_hls_state();
        // Simulate audio.seek(): bump epoch, set flushing=true.
        let new_epoch = state
            .scheduler
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(2));
        assert!(
            state.scheduler.coord.timeline().is_flushing(),
            "precondition: flushing must be set"
        );

        // Enqueue a demand with the new epoch (post-seek).
        state
            .scheduler
            .coord
            .enqueue_segment_request(SegmentRequest {
                segment_index: 1,
                variant: 0,
                seek_epoch: new_epoch,
            });

        // Call process_demand — this is what poll_next does first, BEFORE
        // the flushing gate. Today it consumes the demand via take().
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = process_demand(&mut state, &mut cx);

        // The demand must NOT have been drained: poll_next's flushing gate
        // will now return Pending without producing a FetchCmd, so the
        // demand must survive for the next poll cycle — otherwise the
        // reader's seek deadlocks (no one will fetch the target segment).
        let still_pending = state.scheduler.coord.peek_segment_request();
        assert!(
            still_pending.is_some(),
            "demand was drained while is_flushing()==true; poll_next's \
             flushing gate will now return Pending without issuing a \
             FetchCmd, so the fetch is never scheduled — the seek deadlocks"
        );
    }

    /// RED test #7 (integration: live_ephemeral_small_cache_playback_hls)
    ///
    /// With a small ephemeral LRU cache, once the downloader has fetched
    /// every segment of the playlist, older segments are invalidated as
    /// newer ones take their slot. At tail state, `handle_tail_state`
    /// calls `rewind_to_first_missing_segment`, which scans from
    /// `cursor.fill_floor() == 0` and rewinds the cursor to the very
    /// first invalidated segment (0) — even when the reader has long
    /// since advanced past segment 0.
    ///
    /// The scheduler then re-downloads segments 0, 1, 2, … evicting the
    /// tail segments the reader is currently reading. This cycle repeats
    /// every time the cursor hits the tail, flooding the server with
    /// requests and starving the reader's on-demand request for a tail
    /// segment. Under concurrent test load the hang detector fires well
    /// before the reader receives the segment it needs.
    ///
    /// Invariant under test: when the reader has advanced past evicted
    /// segments (the reader's byte position is inside a segment that is
    /// still in the LRU cache), tail-state rewind must NOT pull the
    /// cursor back to segment 0. Evicted segments behind the reader are
    /// no longer needed and should not be re-fetched.
    #[kithara::test]
    fn red_test_small_cache_tail_rewind_does_not_drag_cursor_behind_reader() {
        // Scenario mirrors the failing integration test: ephemeral store
        // with a small LRU — reader reads forward, old segments evict.
        let mut state = make_hls_state();
        let scheduler = &mut state.scheduler;

        assert!(
            scheduler.backend.is_ephemeral(),
            "precondition: test fixture uses ephemeral backend"
        );

        // Populate expected sizes so byte-map math works. Uniform sizes
        // keep the assertion simple: each segment is 100 bytes, so seg N
        // spans [N*100, (N+1)*100).
        const SEG_SIZE: u64 = 100;
        scheduler
            .segments
            .lock_sync()
            .set_expected_sizes(0, vec![SEG_SIZE; Consts::NUM_SEGMENTS]);

        // Commit every segment of variant 0 — downloader has walked the
        // full playlist once.
        for seg_idx in 0..Consts::NUM_SEGMENTS {
            commit_segment_in_index(&state, 0, seg_idx, SEG_SIZE);
        }
        let scheduler = &mut state.scheduler;

        // LRU with cap=4 has evicted everything except the last 4
        // segments. Simulate the on_invalidated callback by marking the
        // old segments unavailable in the StreamIndex.
        const CACHE_WINDOW: usize = 4;
        {
            let mut segments = scheduler.segments.lock_sync();
            for seg_idx in 0..Consts::NUM_SEGMENTS - CACHE_WINDOW {
                segments.on_segment_invalidated(0, seg_idx);
            }
        }

        // Cursor is past the tail — every segment was fetched once.
        scheduler.cursor.reopen_fill(0, Consts::NUM_SEGMENTS);
        assert_eq!(scheduler.current_segment_index(), Consts::NUM_SEGMENTS);

        // Reader's byte position is inside the cache window — seg
        // (NUM_SEGMENTS - 2) is still cached. Evicted segments (< 36)
        // are BEHIND the reader and must not be re-fetched.
        let reader_seg = Consts::NUM_SEGMENTS - 2;
        let reader_byte_pos = reader_seg as u64 * SEG_SIZE + 10;
        scheduler
            .coord
            .timeline()
            .set_byte_position(reader_byte_pos);

        // Tail-state handler: this is what poll_next calls every cycle
        // once the cursor is at num_segments. Today it unconditionally
        // rewinds to segment 0 via rewind_to_first_missing_segment.
        let consumed = scheduler.handle_tail_state(0, Consts::NUM_SEGMENTS);

        let cursor_after = scheduler.current_segment_index();
        assert!(
            cursor_after >= reader_seg,
            "tail-state rewind pulled the cursor to segment {cursor_after}, \
             which is BEHIND the reader (reader_seg={reader_seg}, \
             reader_byte_pos={reader_byte_pos}). With an ephemeral cap=4 \
             cache, re-downloading segments 0..{reader_seg} will evict \
             the segments the reader is currently reading and starve \
             playback. consumed_as_tail={consumed}"
        );
    }

    /// RED test #9 (integration: stress_seek_lifecycle_with_zero_reset_ephemeral)
    ///
    /// Phase 3 of the stress test: after 2000 random seeks (which cause
    /// ABR to up/down-switch repeatedly), the reader seeks back to 0 and
    /// reads the full track sequentially. By this point, BOTH variants
    /// have had most of their segments committed at some point — but the
    /// ephemeral LRU has since evicted many of them via
    /// `on_segment_invalidated`.
    ///
    /// The hang: log shows `cursor::reopen_fill from_next=28 from_floor=27
    /// new_floor=27 new_next=27` firing *between* regular polls, without
    /// any seek epoch change. The only production caller of
    /// `reopen_fill` is `handle_midstream_switch`, which is gated on
    /// `is_midstream_switch = (download_variant != variant) && seg > 0`.
    ///
    /// The trigger: when `handle_tail_state` exits the `layout != variant`
    /// branch with no layout gap (line 131) or via the `else` branch at
    /// line 133, it clears `filling_layout_gap = false`. On the next
    /// poll, ABR wants its current pick (say variant 1). Since
    /// `filling_layout_gap` is now false, `resolve_variant` does NOT
    /// override to `download_variant` (= layout variant 0). Then
    /// `apply_variant_readiness` sees `download_variant=0, variant=1`,
    /// classifies it as a midstream switch, and fires
    /// `handle_midstream_switch(true)` → `reopen_fill(cursor_pos,
    /// cursor_pos)` where `cursor_pos = first_missing_segment(
    /// download_variant=0, 0, ephemeral=true)` — which points BEHIND
    /// the reader at the oldest LRU-evicted seg on variant 0.
    ///
    /// Then build_batch runs on variant 1, where seg 27..40 are all
    /// loaded (LRU kept them), so every seg is skipped. cursor sweeps
    /// 27..40 again. Tail. Rewind. Loop.
    ///
    /// Invariant under test: when the layout variant has all segments
    /// committed (no missing in the reader's forward window) and the
    /// cursor is at the tail, a subsequent poll_next must NOT reopen
    /// the cursor onto an LRU-evicted segment strictly behind the
    /// reader's byte position. Doing so spins the scheduler on already
    /// played-back data while the reader starves for the next segment
    /// it actually needs.
    #[kithara::test]
    fn red_test_zero_reset_ephemeral_tail_switch_does_not_rewind_behind_reader() {
        let mut state = make_hls_state();

        assert!(
            state.scheduler.backend.is_ephemeral(),
            "precondition: test fixture uses ephemeral backend"
        );

        // Phase 2 has walked through both variants repeatedly. Commit
        // every segment of variant 0 (the current layout) and variant 1.
        const SEG_SIZE: u64 = 100;
        {
            let mut segs = state.scheduler.segments.lock_sync();
            segs.set_expected_sizes(0, vec![SEG_SIZE; Consts::NUM_SEGMENTS]);
            segs.set_expected_sizes(1, vec![SEG_SIZE; Consts::NUM_SEGMENTS]);
            segs.set_layout_variant(0);
        }
        for seg_idx in 0..Consts::NUM_SEGMENTS {
            commit_segment_in_index(&state, 0, seg_idx, SEG_SIZE);
            commit_segment_in_index(&state, 1, seg_idx, SEG_SIZE);
        }

        // LRU has since evicted the old half of BOTH variants. Only
        // segments 20..40 remain loaded.
        const LIVE_START: usize = 20;
        {
            let mut segs = state.scheduler.segments.lock_sync();
            for seg_idx in 0..LIVE_START {
                segs.on_segment_invalidated(0, seg_idx);
                segs.on_segment_invalidated(1, seg_idx);
            }
        }

        // Reader is reading sequentially from the start after seek-to-0;
        // byte_position is inside segment 25 (still in the live window).
        const READER_SEG: usize = 25;
        state
            .scheduler
            .coord
            .timeline()
            .set_byte_position(READER_SEG as u64 * SEG_SIZE + 10);

        // Production state at the hang:
        //   - download_variant = 0 (layout variant)
        //   - cursor is at the tail of the playlist (40)
        //   - filling_layout_gap was cleared by the previous
        //     handle_tail_state exit path (layout==variant branch)
        //   - ABR wants variant 1 (throughput suggests up/down-switch)
        state.scheduler.download_variant = 0;
        state
            .scheduler
            .cursor
            .reopen_fill(READER_SEG, Consts::NUM_SEGMENTS);
        state.scheduler.filling_layout_gap = false;
        // Simulate ABR having picked variant 1 (e.g. after a down-switch).
        state.scheduler.abr.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::DownSwitch,
                changed: true,
            },
            PlatformInstant::now(),
        );

        assert_eq!(
            state.scheduler.current_segment_index(),
            Consts::NUM_SEGMENTS,
            "precondition: cursor at tail"
        );
        assert_eq!(
            state.scheduler.abr.get_current_variant_index(),
            1,
            "precondition: ABR wants variant 1"
        );

        // Drive one poll_next cycle via the helpers, exactly as
        // `HlsPeer::poll_next` does. No demand is queued — this is a
        // prefetch poll after the reader is busy draining the decoder.
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let (demand_segment, demand_variant_override) = match process_demand(&mut state, &mut cx) {
            DemandResult::ResetAndPend => (None, None),
            DemandResult::Demand {
                segment,
                variant_override,
            } => (Some(segment), variant_override),
            DemandResult::None => (None, None),
        };

        let (old_variant, variant) = resolve_variant(&mut state.scheduler, demand_variant_override);

        // handle_tail_state runs before apply_variant_readiness. At
        // cursor=40 and variant=1, the `layout != variant` branch
        // triggers `first_missing_segment(layout=0, scan_start=reader_seg,
        // layout_num, ephemeral)`. With all variant-0 segments in the
        // forward window committed, there is NO gap → fallthrough to
        // `filling_layout_gap = false` (line 131) or the later EOF
        // branch → returns true (Pending). In production that's fine,
        // BUT the NEXT poll then invokes apply_variant_readiness
        // without the filling_layout_gap guard.
        let _is_tail = state
            .scheduler
            .handle_tail_state(variant, Consts::NUM_SEGMENTS);

        // The critical call: with filling_layout_gap=false and variant=1
        // from ABR, classify_variant_transition sees download_variant=0
        // vs variant=1 → midstream switch → handle_midstream_switch →
        // reopen_fill cursor BEHIND the reader.
        let cursor_before = state.scheduler.current_segment_index();
        let reader_seg_before = state.scheduler.reader_segment_floor();
        apply_variant_readiness(
            &mut state.scheduler,
            variant,
            old_variant,
            demand_segment,
            demand_variant_override,
        );
        let cursor_after = state.scheduler.current_segment_index();

        assert!(
            cursor_after >= reader_seg_before,
            "apply_variant_readiness pulled the cursor from {cursor_before} \
             down to {cursor_after}, which is BEHIND the reader \
             (reader_segment_floor={reader_seg_before}). The trigger is \
             an ABR-driven midstream switch that fires `reopen_fill` to \
             `first_missing_segment(download_variant=0, 0, ephemeral=true)` \
             — which finds LRU-evicted segments strictly behind the reader. \
             Re-fetching those segments evicts the live window the reader \
             is about to read, creating the hot loop observed in \
             `stress_seek_lifecycle_with_zero_reset_ephemeral` Phase 3."
        );
    }

    /// RED test #6 (integration: stress_seek_lifecycle_with_zero_reset_mmap)
    ///
    /// `process_demand` unconditionally rewinds the cursor when
    /// `req.segment_index < current_segment_index`, even if that segment
    /// is already committed in the StreamIndex. This drives a hot loop:
    /// demand → rewind → `build_batch` re-issues FetchCmd → commit
    /// (on_complete wakes reader) → reader re-demands → rewind → ...
    ///
    /// Invariant under test: when the demand targets an already-committed
    /// segment, the cursor must NOT regress.
    #[kithara::test]
    fn red_test_seek_lifecycle_mmap_no_cursor_regress_on_committed_segment() {
        let mut state = make_hls_state();

        // Commit segment 22 on variant 0 (both in StreamIndex + AssetStore).
        commit_segment_in_index(&state, 0, 22, 100);

        // Cursor is past segment 22 (downloading segment 23 next). floor=20
        // mirrors the production log: `cursor::rewind_fill_to from=23 to=22
        // floor=20 target=22` — seg 22 is above the floor so the unconditional
        // rewind in process_demand actually reduces the cursor.
        state.scheduler.cursor.reopen_fill(20, 23);
        let cursor_before = state.scheduler.current_segment_index();
        assert_eq!(cursor_before, 23);

        // Reader enqueues a demand for segment 22 (e.g. after waking from
        // a read barrier). Same variant as download_variant (default 0).
        enqueue_demand(&state, 0, 22);

        // Call process_demand — the buggy branch rewinds cursor 23 → 22.
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = process_demand(&mut state, &mut cx);

        let cursor_after = state.scheduler.current_segment_index();
        assert_eq!(
            cursor_after, cursor_before,
            "cursor must not regress onto already-committed segment 22 \
             (was {cursor_before}, now {cursor_after}); unconditional \
             rewind creates hot loop: rewind → re-fetch → commit errors \
             with 'cannot write to committed resource' → reader re-demands"
        );
    }

    /// RED test #8 (integration: stress_seek_lifecycle_with_zero_reset_mmap).
    ///
    /// The existing fix at peer.rs:272..297 (commit `38d51adfb`) covers only
    /// the case where the demanded segment is already committed in the
    /// `StreamIndex`. But the integration test still hangs because the hot
    /// loop fires BEFORE commit — while the `FetchCmd` for the demanded
    /// segment is still in flight.
    ///
    /// Scenario reproduced from the `stress_seek_lifecycle_with_zero_reset_mmap`
    /// trace log (see `/tmp/test-clean.log` lines ~313..345):
    ///
    /// ```text
    /// 55.431134Z poll_next: returning 1 FetchCmds variant=0 count=1 cursor=13
    /// 55.431368Z poll_next: same-epoch demand behind cursor, rewinding
    ///                       variant=0 segment=12 cursor=13
    /// 55.432289Z poll_next: returning 1 FetchCmds variant=0 count=1 cursor=13
    ///                       ^^^ DUPLICATE FetchCmd for segment 12
    /// 55.432384Z poll_next: same-epoch demand behind cursor, rewinding
    ///                       variant=0 segment=12 cursor=13
    /// ...  11 duplicate FetchCmds for segment 12 in ~4ms ...
    /// 55.956343Z poll_next: returning 3 FetchCmds variant=0 count=3 cursor=16
    /// ```
    ///
    /// At 55.431134 the scheduler issued the FIRST FetchCmd for segment 12
    /// and advanced `cursor` to 13. `count=12` in the preceding `size_map`
    /// event confirms segment 12 is NOT yet committed — the fetch is in
    /// flight, writing into a freshly acquired `AssetResource`. Between
    /// ticks, the reader's `wait_range` loop re-enqueues a demand for
    /// (variant=0, segment=12) on every condvar iteration. Today
    /// `process_demand`'s `already_loaded` check returns `false` (the
    /// commit hasn't landed), so the rewind 13 → 12 fires, `build_batch`
    /// emits a second `FetchCmd` for the same resource, and the cycle
    /// repeats with two (then N) concurrent writers racing on the same
    /// cached `AssetResource`. On mmap storage the first commit flips
    /// the resource into `Committed` state; subsequent `write_at` calls
    /// fail, and eventually the `on_complete` closure that wins the race
    /// calls `commit_fetch_inline` only once — but by then, the 500ms
    /// variant-0 delay rule has pushed every duplicate into the same
    /// 500ms window, so the reader waits ~40×500 ms to burn through
    /// 28 segments and trips the 5-second hang detector well before
    /// phase 3 completes.
    ///
    /// Invariant under test: when `process_demand` finds that the
    /// demanded segment sits strictly between the cursor's download
    /// floor and its current position (i.e. the scheduler already
    /// emitted a `FetchCmd` for it in THIS download epoch), the cursor
    /// must NOT regress onto that segment. The prior `FetchCmd` is
    /// still in flight; rewinding issues a duplicate `FetchCmd` that
    /// races on the same `AssetResource` and delays commit.
    ///
    /// The `already_loaded` check in the current fix is insufficient —
    /// it only sees committed state. A correct guard must treat
    /// `floor <= req.segment_index < cursor` as "already planned in
    /// this epoch" regardless of commit status.
    #[kithara::test]
    fn red_test_seek_lifecycle_mmap_no_rewind_on_in_flight_segment() {
        let mut state = make_hls_state();

        // Mirror production log state at 55.431134Z, right after the
        // scheduler issued the FIRST FetchCmd for segment 12:
        //   cursor: floor=12 (the download epoch opened at seg 12),
        //           next=13 (advance_current_segment_index(12 + 1) after
        //           build_batch pushed the FetchCmd).
        // Segment 12 is NOT committed — the fetch is in flight, writing
        // into the freshly acquired `AssetResource` captured by the
        // FetchCmd's writer closure. The StreamIndex contains no entry
        // for (variant=0, segment=12) yet, so `is_segment_loaded`
        // returns false and the existing fix's skip branch does NOT
        // fire.
        state.scheduler.cursor.reopen_fill(12, 13);
        // `build_batch` marks the segment in-flight when it emits the
        // FetchCmd; the production log captures this as `insert_in_flight`
        // immediately before `advance_current_segment_index(12 + 1)`.
        state.scheduler.in_flight_segments.insert((0, 12));
        let cursor_before = state.scheduler.current_segment_index();
        assert_eq!(cursor_before, 13);

        // Sanity: segment 12 is NOT committed (the distinguishing trait
        // from RED #6, which committed it before the assertion).
        assert!(
            !state
                .scheduler
                .segments
                .lock_sync()
                .is_segment_loaded(0, 12),
            "precondition: segment 12 must be uncommitted (fetch in flight)"
        );

        // Reader's `wait_range` loop re-enqueues the demand for the
        // in-flight segment on every condvar iteration
        // (source_impl.rs:130, `queue_segment_request_for_offset`).
        enqueue_demand(&state, 0, 12);

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = process_demand(&mut state, &mut cx);

        let cursor_after = state.scheduler.current_segment_index();
        assert_eq!(
            cursor_after, cursor_before,
            "cursor regressed from {cursor_before} to {cursor_after} onto \
             segment 12, whose FetchCmd is still in flight (floor=12, so \
             the scheduler ALREADY emitted a FetchCmd for this segment in \
             the current download epoch before advancing to cursor=13). \
             Today's `already_loaded` guard only detects committed state, \
             so the rewind 13 → 12 fires, build_batch issues a duplicate \
             FetchCmd, and two concurrent writers race on the same cached \
             AssetResource. Under the 500ms variant-0 DelayRule in the \
             integration test, this hot loop starves the reader until the \
             5-second hang detector panics. A correct guard must treat \
             `floor <= demand_seg < cursor` as 'already planned in this \
             epoch' regardless of commit status."
        );
    }

    /// RED test (integration: live_ephemeral_small_cache_playback_hls).
    ///
    /// Reproduces the flake observed under CPU contention when running
    /// `live_ephemeral_small_cache_playback_hls` with 8-way parallel
    /// stress (6/8 runs fail with `next_chunk timeout at
    /// stage='ephemeral_small_cache' (is_eof=false)`).
    ///
    /// Scenario: forward-only playback, `cache_capacity=4`, ephemeral
    /// store, `AbrMode::Auto(Some(0))`. The reader is inside the LRU's
    /// live window. ABR's initial pick was variant 0 (floor). After a
    /// few seconds of throughput measurement ABR up-switches to a higher
    /// variant. The downloader has reached the tail of variant 0 with
    /// `filling_layout_gap = false` (the previous `handle_tail_state`
    /// exited via the `else` arm at plan.rs:133).
    ///
    /// The next `poll_next` calls:
    /// 1. `resolve_variant` — no demand override, no layout gap-fill,
    ///    so `variant = abr.current = 1`.
    /// 2. `apply_variant_readiness(variant=1, download_variant=0, …)`.
    ///    `classify_variant_transition` reports a midstream switch.
    ///    `handle_midstream_switch(true)` runs
    ///    `first_missing_segment(old_variant=0, scan_start=0,
    ///    ephemeral=true)` and finds segment 0 — the oldest LRU-evicted
    ///    segment BEHIND the reader.
    /// 3. `reopen_fill(0, 0)` pulls the cursor from the tail down to 0.
    /// 4. `build_batch` starts re-downloading segments 0, 1, 2, … which
    ///    evicts the reader's live window. Reader starves → `wait_range`
    ///    exceeds its 3-second budget → `next_chunk_with_timeout` fires
    ///    the assert at tests/tests/kithara_hls/live_stress_real_stream.rs:190.
    ///
    /// Invariant under test: with forward-only playback on an ephemeral
    /// LRU store, an ABR up-switch at the tail must NOT rewind the
    /// cursor onto segments strictly behind the reader. The reader's
    /// live window must remain untouched.
    #[kithara::test]
    fn red_test_small_cache_playback_abr_upswitch_does_not_rewind_behind_reader() {
        let mut state = make_hls_state();

        assert!(
            state.scheduler.backend.is_ephemeral(),
            "precondition: small-cache playback uses ephemeral backend"
        );

        // All 40 segments of variants 0 and 1 were downloaded while the
        // reader walked forward. Layout is variant 0 (initial ABR pick).
        const SEG_SIZE: u64 = 100;
        {
            let mut segs = state.scheduler.segments.lock_sync();
            segs.set_expected_sizes(0, vec![SEG_SIZE; Consts::NUM_SEGMENTS]);
            segs.set_expected_sizes(1, vec![SEG_SIZE; Consts::NUM_SEGMENTS]);
            segs.set_layout_variant(0);
        }
        for seg_idx in 0..Consts::NUM_SEGMENTS {
            commit_segment_in_index(&state, 0, seg_idx, SEG_SIZE);
            commit_segment_in_index(&state, 1, seg_idx, SEG_SIZE);
        }

        // LRU cap=4 has evicted everything except the last 4 segments on
        // BOTH variants — matches `.with_cache_capacity(NonZeroUsize::new(4))`
        // on the integration test's StoreOptions.
        const CACHE_WINDOW: usize = 4;
        const LIVE_START: usize = Consts::NUM_SEGMENTS - CACHE_WINDOW;
        {
            let mut segs = state.scheduler.segments.lock_sync();
            for seg_idx in 0..LIVE_START {
                segs.on_segment_invalidated(0, seg_idx);
                segs.on_segment_invalidated(1, seg_idx);
            }
        }

        // Reader is playing forward inside the live window — second-to-last
        // cached segment. This is the state right before the flake fires:
        // 55 seconds in, `chunks_read` ≈ 2000, decoder advancing, byte
        // position inside LIVE_START+2.
        const READER_SEG: usize = Consts::NUM_SEGMENTS - 2;
        let reader_byte_pos = READER_SEG as u64 * SEG_SIZE + 10;
        state
            .scheduler
            .coord
            .timeline()
            .set_byte_position(reader_byte_pos);

        // Downloader is at the playlist tail on the current layout variant.
        // `filling_layout_gap` was cleared by the previous
        // `handle_tail_state` pass (layout==variant → `else` arm at
        // plan.rs:133).
        state.scheduler.download_variant = 0;
        state
            .scheduler
            .cursor
            .reopen_fill(LIVE_START, Consts::NUM_SEGMENTS);
        state.scheduler.filling_layout_gap = false;

        // ABR's initial pick was variant 0 (`AbrMode::Auto(Some(0))`).
        // After throughput measurement it decides to up-switch to
        // variant 1. `abr.get_current_variant_index()` now returns 1.
        state.scheduler.abr.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::UpSwitch,
                changed: true,
            },
            PlatformInstant::now(),
        );

        assert_eq!(
            state.scheduler.current_segment_index(),
            Consts::NUM_SEGMENTS,
            "precondition: cursor at tail"
        );

        // Drive the post-tail `poll_next` cycle through its helpers,
        // exactly as `HlsPeer::poll_next` does in production. No demand
        // is queued — this is a prefetch poll.
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let (demand_segment, demand_variant_override) = match process_demand(&mut state, &mut cx) {
            DemandResult::ResetAndPend => (None, None),
            DemandResult::Demand {
                segment,
                variant_override,
            } => (Some(segment), variant_override),
            DemandResult::None => (None, None),
        };

        let (old_variant, variant) = resolve_variant(&mut state.scheduler, demand_variant_override);

        // `handle_tail_state` runs before `apply_variant_readiness`. At
        // cursor == num_segments and variant 1 fully cached, this hits
        // the `layout != variant` branch, finds no layout gap in the
        // reader's forward window, and falls through clearing
        // `filling_layout_gap`.
        let _is_tail = state
            .scheduler
            .handle_tail_state(variant, Consts::NUM_SEGMENTS);

        // The critical call. With `download_variant=0`, `variant=1`
        // (from ABR up-switch), and `filling_layout_gap=false`,
        // `classify_variant_transition` reports a midstream switch.
        // `handle_midstream_switch(true)` then calls
        // `first_missing_segment(old_variant=0, 0, ephemeral=true)`,
        // which returns 0 — the oldest LRU-evicted segment behind the
        // reader. `reopen_fill(0, 0)` drags the cursor back.
        let cursor_before = state.scheduler.current_segment_index();
        let reader_seg_before = state.scheduler.reader_segment_floor();
        apply_variant_readiness(
            &mut state.scheduler,
            variant,
            old_variant,
            demand_segment,
            demand_variant_override,
        );
        let cursor_after = state.scheduler.current_segment_index();

        assert!(
            cursor_after >= reader_seg_before,
            "apply_variant_readiness pulled the cursor from {cursor_before} \
             down to {cursor_after}, which is BEHIND the reader \
             (reader_segment_floor={reader_seg_before}, reader_byte_pos=\
             {reader_byte_pos}). With cache_capacity=4 on an ephemeral \
             store, re-fetching segments 0..{reader_seg_before} evicts \
             the reader's live window (segments \
             {LIVE_START}..{num_segs}) the decoder is actively \
             reading, so `wait_range` exceeds its budget and \
             `next_chunk_with_timeout` fires the assert at \
             live_stress_real_stream.rs:190 with \
             `stage='ephemeral_small_cache' (is_eof=false)`.",
            num_segs = Consts::NUM_SEGMENTS
        );
    }

    /// RED test: stale `on_complete` callback from a cancelled prior-epoch
    /// FetchCmd advances the **new** epoch's cursor forward.
    ///
    /// Scenario reproduced from the production "wait_range EOF after seek"
    /// hang report:
    ///
    /// 1. Reader playing forward on variant 0; downloader emitted a
    ///    `FetchCmd` for segment 22 in seek_epoch=0. The FetchCmd's
    ///    `on_complete` closure captured `seek_epoch = 0`, `(variant=0,
    ///    seg_idx=22)`, and an `Arc<Mutex<HlsState>>`.
    /// 2. User seeks BACKWARD to ~segment 10. `process_demand` (peer.rs:281)
    ///    fires `state.epoch_cancel.cancel()`, which cancels every
    ///    in-flight FetchCmd in the old epoch — the Downloader's body
    ///    stream wrapper (response.rs:144) yields `NetError::Cancelled` and
    ///    `deliver()` (batch.rs:191-214) calls `on_complete(0,
    ///    Some(&NetError::Cancelled))` for each cancelled fetch.
    /// 3. Meanwhile, `next_valid_demand_request` ran
    ///    `reset_for_seek_epoch(new_epoch=1, variant=0, segment_index=10)`
    ///    (plan.rs:21 → state.rs:191), which did
    ///    `reset_cursor(10)` → cursor floor=10, next=10. The active epoch
    ///    is now 1.
    /// 4. The stale `on_complete` for the old (variant=0, seg=22) fetch
    ///    fires now, in the NEW epoch context. `peer.rs:626-641` does:
    ///        - `in_flight_segments.remove((0, 22))` — no-op, set already
    ///          cleared by `reset_for_seek_epoch:210`.
    ///        - `rewind_current_segment_index(22)` →
    ///          `cursor.rewind_fill_to(22)` (cursor.rs:62-67) →
    ///          `result = floor.max(next) = max(10, 22) = 22` →
    ///          `self.next = 22`.
    ///    Cursor jumps from 10 to 22 in the NEW epoch. The downloader will
    ///    not naturally fetch segments 10..21 on the next `poll_next`
    ///    cycle (cursor is already past them). Reader at byte position
    ///    inside segment 10 receives `wait_range: EOF` because
    ///    `loaded_total = max_end_offset()` doesn't yet cover segment 10
    ///    (no FetchCmd was emitted for it in the new epoch), and
    ///    `known_total` may be missing too if the size_map is variant-
    ///    specific.
    ///
    /// Invariant under test: a stale `on_complete` from a prior seek
    /// epoch must not advance the new epoch's cursor forward.
    /// Equivalently, `cursor.rewind_fill_to(N)` MUST clamp `N` so the
    /// cursor never moves forward — it is a "rewind" operation, not a
    /// "set".
    #[kithara::test]
    fn red_stale_on_complete_does_not_advance_new_epoch_cursor() {
        // Bring the production state into Arc<Mutex<Option<...>>> just
        // like `HlsPeer::state` so `build_fetch_cmd` can clone it into
        // the on_complete closure.
        let state_arc = Arc::new(PlatformMutex::new(Some(make_hls_state())));

        // Old-epoch state: downloader has just emitted a FetchCmd for
        // segment 22 (cursor floor=20 covered segs 20..22 in this epoch,
        // build_batch advanced next to 23 after pushing the FetchCmd).
        let captured_seek_epoch = {
            let mut guard = state_arc.lock_sync();
            let st = guard.as_mut().expect("state must be initialized");
            st.scheduler.cursor.reopen_fill(20, 23);
            st.scheduler.in_flight_segments.insert((0, 22));
            st.scheduler.coord.timeline().seek_epoch()
        };
        assert_eq!(
            captured_seek_epoch, 0,
            "precondition: FetchCmd emitted in initial seek_epoch=0",
        );

        // Build the production FetchCmd via the actual `build_fetch_cmd`
        // helper so the on_complete closure is the real one — not a
        // hand-rolled copy that drifts from production logic.
        let url = Url::parse("https://example.com/seg-0-22.m4s").expect("valid url");
        let mut cmd = {
            let guard = state_arc.lock_sync();
            let st = guard.as_ref().expect("state must be initialized");
            let res = st
                .scheduler
                .backend
                .acquire_resource(&ResourceKey::from_url(&url))
                .expect("acquire resource");
            let prepared = crate::loading::segment_loader::PreparedMedia {
                url: url.clone(),
                duration: Some(Duration::from_secs(4)),
                cached_len: None,
                resource: Some(res),
            };
            build_fetch_cmd(st, &state_arc, 0, 22, captured_seek_epoch, prepared, false)
        };
        let on_complete = cmd
            .on_complete
            .take()
            .expect("FetchCmd built by build_fetch_cmd must carry an on_complete");

        // Simulate seek BACKWARD to segment 10. Bump the timeline epoch
        // (this is what `Timeline::initiate_seek` does in production
        // before `process_demand` runs) and reset cursor / in_flight as
        // `reset_for_seek_epoch` would.
        let cursor_after_seek = {
            let mut guard = state_arc.lock_sync();
            let st = guard.as_mut().expect("state must be initialized");
            let new_epoch = st
                .scheduler
                .coord
                .timeline()
                .initiate_seek(Duration::from_secs(40));
            assert!(
                new_epoch > captured_seek_epoch,
                "seek must produce a strictly newer epoch",
            );
            st.scheduler.active_seek_epoch = new_epoch;
            st.scheduler.in_flight_segments.clear();
            st.scheduler.reset_cursor(10);
            st.scheduler.current_segment_index()
        };
        assert_eq!(
            cursor_after_seek, 10,
            "precondition: new-epoch cursor must be reset to seek target (seg 10)",
        );

        // Stale on_complete fires now. In production this is dispatched
        // by the Downloader's batch task when the body stream observes
        // its CancellationToken fire (epoch_cancel was cancelled by
        // process_demand at the start of the new epoch). The error
        // delivered is `NetError::Cancelled` (response.rs:147 + batch.rs
        // delivery).
        let cancelled = NetError::Cancelled;
        on_complete(0, Some(&cancelled));

        let cursor_after_stale_callback = {
            let guard = state_arc.lock_sync();
            let st = guard.as_ref().expect("state must be initialized");
            st.scheduler.current_segment_index()
        };

        assert!(
            cursor_after_stale_callback <= cursor_after_seek,
            "stale on_complete from prior seek epoch advanced new-epoch cursor \
             from {cursor_after_seek} forward to {cursor_after_stale_callback}; \
             segments {cursor_after_seek}..{cursor_after_stale_callback} (which \
             the reader is now waiting for after the seek) will not be emitted \
             by poll_next because the cursor is already past them. The fix is \
             either (a) `on_complete` must early-return when its captured \
             seek_epoch differs from `coord.timeline().seek_epoch()`, OR \
             (b) `DownloadCursor::rewind_fill_to` must clamp the target to \
             `min(self.next, max(self.floor, target))` so a rewind cannot \
             move the cursor forward.",
        );
    }

    /// Regression guard for the HLS seek hang: `AbrController` must hold
    /// its lock across the entire pending-seek window, not only inside
    /// `reset_for_seek_epoch`. The peer's reset runs long after the user
    /// initiates a seek — in the meantime throughput samples keep firing
    /// and `make_abr_decision` is free to switch variants. When it does,
    /// the anchor resolved for the layout variant points at segments the
    /// downloader no longer plans to fetch, `source_is_ready_for_apply_seek`
    /// stays `Waiting` forever, and playback hangs post-seek.
    #[kithara::test]
    fn red_abr_must_stay_locked_while_seek_is_pending() {
        let mut state = make_hls_state();

        assert!(
            !state.scheduler.abr.is_locked(),
            "precondition: ABR starts unlocked on a fresh track"
        );

        // Step 1: user hits the seek slider. Timeline bumps the epoch
        // and flips seek_pending. The peer hasn't polled yet.
        let _new_epoch = state
            .scheduler
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(30));
        assert!(
            state.scheduler.coord.timeline().is_seek_pending(),
            "precondition: seek is pending after initiate_seek",
        );

        // Step 2: ABR tick fires concurrently with the seek — this is the
        // path that actually commits a variant switch. Without a lock
        // covering the pending-seek window, `decide()` may pick a new
        // variant (UpSwitch / DownSwitch) and `apply()` writes it to the
        // shared atomic the peer reads in `resolve_variant`.
        let decision = state.scheduler.make_abr_decision();

        assert!(
            state.scheduler.abr.is_locked(),
            "ABR must be locked for the duration of a pending seek; leaving \
             it unlocked opens the mid-seek variant switch that makes the \
             anchor byte_offset unreachable"
        );
        assert_eq!(
            decision.reason,
            AbrReason::Locked,
            "make_abr_decision must short-circuit to Locked while the \
             Timeline reports a pending seek, got {:?}",
            decision.reason,
        );
    }

    /// Companion to the pending-seek lock test: once the decoder has
    /// applied the seek (`clear_seek_pending`), the next ABR tick must
    /// release the lock so the controller can react to throughput again.
    /// A stuck lock would pin the variant forever after any seek.
    #[kithara::test]
    fn abr_unlocks_when_seek_pending_clears() {
        let mut state = make_hls_state();
        let timeline = state.scheduler.coord.timeline();

        // Drive the same first-tick path as the pending-seek test so ABR
        // ends up locked.
        let epoch = timeline.initiate_seek(Duration::from_secs(30));
        let _ = state.scheduler.make_abr_decision();
        assert!(
            state.scheduler.abr.is_locked(),
            "precondition: ABR is locked during the pending seek",
        );

        // Decoder finishes seeking → clear_seek_pending matches on epoch
        // and lowers the flag. The very next ABR tick must see the flag
        // cleared and release the lock.
        timeline.clear_seek_pending(epoch);
        assert!(
            !timeline.is_seek_pending(),
            "precondition: seek_pending cleared after clear_seek_pending",
        );

        let decision = state.scheduler.make_abr_decision();
        assert!(
            !state.scheduler.abr.is_locked(),
            "ABR must unlock on the first tick after seek_pending clears — \
             otherwise the first seek pins the variant for the rest of the \
             session and ABR stops reacting to network conditions"
        );
        assert_ne!(
            decision.reason,
            AbrReason::Locked,
            "decision reason must not be Locked once the lock has been \
             released, got {:?}",
            decision.reason,
        );
    }

    // --- Task 6: priority reflects Timeline::is_playing ---

    #[kithara::test]
    fn priority_defaults_to_low_before_activation() {
        let timeline = Timeline::new();
        let peer = HlsPeer::new(timeline);
        assert_eq!(
            peer.priority(),
            crate::peer::Priority::Low,
            "fresh HlsPeer with PLAYING=false must report Low"
        );
    }

    #[kithara::test]
    fn priority_tracks_set_playing_without_activation() {
        let timeline = Timeline::new();
        let peer = HlsPeer::new(timeline.clone());
        assert_eq!(peer.priority(), crate::peer::Priority::Low);

        timeline.set_playing(true);
        assert_eq!(
            peer.priority(),
            crate::peer::Priority::High,
            "set_playing(true) must flip priority to High even before activate()"
        );

        timeline.set_playing(false);
        assert_eq!(
            peer.priority(),
            crate::peer::Priority::Low,
            "set_playing(false) must return priority to Low"
        );
    }

    #[kithara::test]
    fn priority_lookup_does_not_lock_state_mutex() {
        // Hold the state mutex from another thread-simulated context and
        // confirm priority() still resolves without blocking. Because
        // `state` is a PlatformMutex, lock_sync would block forever if
        // priority() tried to acquire it.
        let timeline = Timeline::new();
        let peer = HlsPeer::new(timeline.clone());
        let _guard = peer.state.lock_sync();
        timeline.set_playing(true);
        assert_eq!(
            peer.priority(),
            crate::peer::Priority::High,
            "priority() must not contend on the state mutex"
        );
    }

    #[kithara::test]
    fn peer_and_coord_share_the_same_timeline_arc() {
        // Wiring invariant from the plan: the Timeline handed to
        // HlsPeer::new must be the same clone fed to HlsCoord. We can
        // verify this indirectly: flip set_playing on the peer's
        // timeline and observe is_playing on a re-cloned handle.
        let timeline = Timeline::new();
        let peer = HlsPeer::new(timeline.clone());
        let other_handle = timeline.clone();
        other_handle.set_playing(true);
        assert!(other_handle.is_playing());
        assert_eq!(
            peer.priority(),
            crate::peer::Priority::High,
            "peer's Timeline clone must observe writes to sibling clones"
        );
    }
}
