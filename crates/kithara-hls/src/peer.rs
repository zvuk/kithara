#![forbid(unsafe_code)]

use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use kithara_abr::{
    Abr, AbrMode, AbrProgressSnapshot, AbrPublisher, AbrReason, AbrState, VariantDuration,
    VariantInfo,
};
use kithara_assets::ResourceKey;
use kithara_bufpool::HasPool;
use kithara_download::{FetchCmd, Peer, RequestPriority};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
    tokio::{
        self,
        sync::mpsc,
        task::{spawn, yield_now},
    },
};
use kithara_stream::{Activity, DeferredWake, SeekObserve, WorkerWake};
use kithara_test_utils::kithara;

use crate::{
    ids::duration_prefix,
    stream::HlsCoord,
    variant::{PlanConfig, PlanCtx},
};

struct HlsTrackState<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    coord: Arc<HlsCoord<S>>,
    /// Reused from the parent [`HlsPeer`]: stores the reader's last-known
    /// segment index — read by [`Abr::progress`] and compared against the
    /// freshly resolved segment in `poll_next` to detect a boundary
    /// crossing. The initial value is set in [`HlsPeer::activate`].
    reader_segment: Arc<AtomicUsize>,
    /// Narrow seek-observe handle — cloned from `HlsPeer.seek_obs` at
    /// activation time. All epoch/target/pending reads use this directly
    /// rather than routing through `coord.timeline`.
    seek_obs: Arc<dyn SeekObserve>,
    /// Target segment of an in-flight forward seek, held until the reader's
    /// physical byte cursor catches up to it. `coord.position()` only
    /// advances when the reader actually reads at the new offset, so right
    /// after a forward seek it still resolves to the *pre-seek* segment until
    /// the decoder recreate's first read lands (the off-RT `wait_range` is now
    /// event-driven, but a native-backend recreate can still take several
    /// scheduler polls). [`Self::apply_seek_change`] already aimed
    /// `reader_segment` + the fetch plan at this target;
    /// [`Self::apply_boundary_crossing`] honours it as a floor so a stale-low
    /// resolved segment cannot drag the cursor back and re-plan the prefix.
    /// Cleared once the reader physically resolves at/after the floor.
    seek_settle_floor: Option<u32>,
    waker: Option<Waker>,
    /// The `HlsConfig`-derived plan config threaded into every `PlanCtx`
    /// this state constructs for `dispatch`.
    config: PlanConfig,
    eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
    last_seek_epoch: u64,
    /// Variant the stored `reader_segment` was resolved against. A
    /// variant switch re-keys the byte space under an unmoved cursor:
    /// the same segment index now points at a different variant's bytes,
    /// so `prev == resolved` no longer proves the reader stayed inside
    /// the planned window. [`HlsTrackState::apply_boundary_crossing`]
    /// treats a variant flip as a discontinuity and re-aims the fetch
    /// plan at the reader's actual segment.
    reader_variant: usize,
}

struct PeerPollWake<S>(Weak<HlsPeer<S>>)
where
    S: HasPool<u8> + Send + Sync + 'static;

impl<S> WorkerWake for PeerPollWake<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn defer(&self) {
        if let Some(peer) = self.0.upgrade() {
            peer.reader_advanced.arm();
        }
    }

    fn wake(&self) {
        if let Some(peer) = self.0.upgrade() {
            peer.wake_poll();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionSlot {
    Active,
    Incoming,
}

impl SessionSlot {
    const fn other(self) -> Self {
        match self {
            Self::Active => Self::Incoming,
            Self::Incoming => Self::Active,
        }
    }
}

#[derive(Default)]
struct SessionTurns {
    incoming: AtomicBool,
}

impl SessionTurns {
    fn next(&self, has_incoming: bool) -> SessionSlot {
        if !has_incoming {
            self.incoming.store(false, Ordering::Relaxed);
            return SessionSlot::Active;
        }
        if self.incoming.fetch_xor(true, Ordering::Relaxed) {
            SessionSlot::Incoming
        } else {
            SessionSlot::Active
        }
    }

    fn reset(&self) {
        self.incoming.store(false, Ordering::Relaxed);
    }
}

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After [`activate`](Self::activate): each `poll_next` drains seek/ABR
/// commit/eviction events and asks the active [`HlsVariant`] for the
/// next batch of `FetchCmd`s (thin event router per spec).
pub(crate) struct HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    abr_publisher: AbrPublisher,
    abr: Arc<AbrState>,
    /// Narrow activity handle. Used by `priority()` to check whether
    /// the track is currently playing.
    activity: Arc<dyn Activity>,
    /// Reader→peer wake channel. The HLS `Source` fires this whenever it
    /// advances the byte cursor or completes a seek, so `poll_next` runs
    /// again without waiting for the next downloader-driven wakeup. Owned
    /// here (not on `HlsCoord`) because the wake mechanism is a property
    /// of the peer, not of shared state.
    reader_advanced: Arc<DeferredWake>,
    reader_segment: Arc<AtomicUsize>,
    /// Narrow seek-observe handle. Used by `poll_next`'s inner logic
    /// (via `HlsTrackState`) to read the current epoch/target without
    /// holding a wide seek/playhead aggregate.
    seek_obs: Arc<dyn SeekObserve>,
    state: Arc<Mutex<Option<HlsTrackState<S>>>>,
    cancel: CancelToken,
    /// Wake-up trigger for the waker-forwarding micro-task: not a
    /// cancellation of work — fires from `teardown()` / `Drop`. A free
    /// `CancelToken` used purely as a one-shot latch (cloned to the
    /// forwarding task; `cancel()` is the fire, idempotent on repeat).
    wake_signal: CancelToken,
    pending_waker: Mutex<Option<Waker>>,
    /// Single source of truth for variant metadata visible to ABR
    /// controller via [`Abr::variants()`] and to UI/FFI via
    /// `AbrHandle::current_variant()`. Populated once by
    /// [`Self::set_abr_variants`] after the master + media playlists
    /// have been parsed; never mutated again for the peer's lifetime.
    variants: Mutex<Vec<VariantInfo>>,
    session_turns: SessionTurns,
}

impl<S> HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn new(
        seek_obs: Arc<dyn SeekObserve>,
        activity: Arc<dyn Activity>,
        initial_mode: AbrMode,
        cancel: CancelToken,
    ) -> Self {
        let abr = Arc::new(AbrState::new(initial_mode));
        let abr_publisher = abr.publisher();
        Self {
            seek_obs,
            activity,
            abr,
            abr_publisher,
            cancel,
            state: Arc::new(Mutex::new(None)),
            pending_waker: Mutex::default(),
            wake_signal: CancelToken::never(),
            variants: Mutex::default(),
            reader_segment: Arc::new(AtomicUsize::new(0)),
            reader_advanced: Arc::new(DeferredWake::default()),
            session_turns: SessionTurns::default(),
        }
    }

    pub(crate) fn abr_publisher(&self) -> AbrPublisher {
        self.abr_publisher.clone()
    }

    /// Lets the `on_slow` hook wake `poll_next` when an in-flight fetch stalls past `soft_timeout`,
    /// so `reconcile_escape` runs without an incidental reader-progress wake. The task yields to
    /// the scheduler after each delivery, since a producer can publish another edge mid-poll,
    /// letting Flash observe quiescence instead of draining `Notify` permits in one poll.
    pub(crate) fn activate(
        self: &Arc<Self>,
        coord: Arc<HlsCoord<S>>,
        eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
        config: PlanConfig,
    ) {
        let reader_advanced = Arc::clone(&self.reader_advanced);
        coord.set_peer_wake(
            Arc::clone(&self.reader_advanced),
            Arc::new(PeerPollWake(Arc::downgrade(self))),
        );
        let cancel = coord.cancel.clone();

        let initial_seg = coord
            .find_at_offset(coord.position())
            .map_or(0, |(idx, _, _)| idx);
        let active = coord.active();
        let plan_ctx = PlanCtx {
            config,
            bus: active.event_bus(),
            scope: coord.scope.clone(),
            headers: coord.headers.clone(),
            seek_epoch: self.seek_obs.epoch(),
            signal: coord.signal(),
        };
        active.rebuild(&plan_ctx, initial_seg);
        self.reader_segment
            .store(initial_seg as usize, Ordering::Release);

        {
            let mut guard = self.state.lock();
            *guard = Some(HlsTrackState {
                reader_variant: coord.variant_index(),
                coord,
                seek_obs: Arc::clone(&self.seek_obs),
                config,
                eviction_rx,
                last_seek_epoch: 0,
                seek_settle_floor: None,
                reader_segment: Arc::clone(&self.reader_segment),
                waker: None,
            });
        }

        let pending_waker = self.pending_waker.lock().take();
        if let Some(waker) = pending_waker {
            waker.wake();
        }

        let peer_weak = Arc::downgrade(self);
        let wake_signal = self.wake_signal.clone();
        spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = wake_signal.cancelled() => return,
                    () = reader_advanced.notified() => {
                        let Some(peer) = peer_weak.upgrade() else { return; };
                        {
                            let guard = peer.state.lock();
                            if let Some(ref state) = *guard
                                && let Some(waker) = state.waker.as_ref()
                            {
                                waker.wake_by_ref();
                            }
                        }
                        yield_now().await;
                    }
                }
            }
        });
    }

    /// Shared wake handle the `Source` clones to resume `poll_next` after a
    /// reader progress event. The reader drivers arm/notify it; this micro-task
    /// awaits [`DeferredWake::notified`].
    pub(crate) fn reader_wake(&self) -> Arc<DeferredWake> {
        Arc::clone(&self.reader_advanced)
    }

    pub(crate) fn set_abr_variants(&self, variants: Vec<VariantInfo>) {
        if let AbrMode::Auto(_) = self.abr.mode()
            && let Some(cap) = self.abr.max_bandwidth_bps()
        {
            let current = self.abr.current_variant_index();
            let current_over_cap = variants
                .iter()
                .find(|variant| variant.variant_index == current)
                .is_none_or(|variant| variant.bandwidth_bps.is_none_or(|bps| bps > cap));
            if current_over_cap {
                let target = variants
                    .iter()
                    .filter(|variant| variant.bandwidth_bps.is_some_and(|bps| bps <= cap))
                    .max_by_key(|variant| variant.bandwidth_bps)
                    .or_else(|| {
                        variants
                            .iter()
                            .min_by_key(|variant| variant.bandwidth_bps.unwrap_or(u64::MAX))
                    });
                if let Some(target) = target {
                    self.abr
                        .request_target(target.variant_index, AbrReason::DownSwitch);
                    if let Some(claim) = self.abr.claim_pending_decision(current) {
                        let _ = self.abr.commit_pending(claim, Instant::now());
                    }
                }
            }
        }
        *self.variants.lock() = variants;
    }

    /// Release the stashed [`HlsTrackState`] and cancel the waker task so
    /// the peer drops its `Arc<HlsCoord>` (and the eviction receiver).
    pub(crate) fn teardown(&self) {
        self.wake_signal.cancel();
        let mut guard = self.state.lock();
        *guard = None;
    }

    /// A wake that finds no waker is silently dropped; the caller has no way to tell that apart
    /// from a wake that landed and produced nothing.
    fn wake_poll(&self) {
        let waker = self
            .state
            .lock()
            .as_ref()
            .and_then(|state| state.waker.clone())
            .or_else(|| self.pending_waker.lock().clone());
        tracing::trace!(found = waker.is_some(), "hls peer wake requested");
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<S> Drop for HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.wake_signal.cancel();
    }
}

impl<S> Abr for HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn cancel(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// `reader_idx`/`download_head` are prefix endpoints into `durations`: `idx == len` is the
    /// valid "at/after the last segment" endpoint and sums the full slice, while `idx > len` is
    /// impossible and surfaces as `None` rather than being silently clamped.
    fn progress(&self) -> Option<AbrProgressSnapshot> {
        let current = self.abr.current_variant_index();
        let durations: Vec<Duration> = self
            .variants
            .lock()
            .iter()
            .find(|v| v.variant_index == current)
            .and_then(|v| match &v.duration {
                VariantDuration::Segmented(d) => Some(d.clone()),
                VariantDuration::Total(_) | VariantDuration::Unknown => None,
            })?;
        let reader_idx = self.reader_segment.load(Ordering::Acquire);
        let download_head = self
            .state
            .lock()
            .as_ref()
            .map_or(0, |s| s.coord.download_head() as usize);
        let reader_playback_time = duration_prefix(&durations, reader_idx)?;
        let download_head_playback_time = duration_prefix(&durations, download_head)?;
        Some(AbrProgressSnapshot {
            download_head_playback_time,
            reader_playback_time,
        })
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.abr))
    }

    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.lock().clone()
    }

    /// The ABR controller runs off the real-time produce core: the peer owns fetch dispatch while
    /// the audio worker owns the exact incoming-session plan, so publishing a decision wakes both
    /// consumers.
    fn wake(&self) {
        let signal = self.state.lock().as_ref().map(|state| state.coord.signal());
        self.reader_advanced.notify_now();
        if let Some(signal) = signal {
            signal.wake_worker();
        }
        self.wake_poll();
    }
}

impl<S> Peer for HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// The active session feeds the speaker while the incoming one is only preparation, so the
    /// active session is served first and a single slot is reserved for the incoming one, so a
    /// switch can never starve the audio currently playing.
    #[kithara::probe]
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let outcome = match self.poll_state_phase(cx) {
            PollPhase::NotActivated => return Poll::Pending,
            PollPhase::Terminated => return Poll::Ready(None),
            PollPhase::Continue(o) => *o,
        };

        for key in outcome.evictions {
            outcome
                .coord
                .broadcast_eviction(&outcome.ctx, &key, outcome.seg_at_reader);
        }

        let mut cmds: Vec<FetchCmd> = Vec::new();
        if outcome.ctx.config.prefetch_budget > 0 {
            let has_incoming = outcome.coord.has_incoming();
            if has_incoming && outcome.ctx.config.prefetch_budget == 1 {
                let first = self.session_turns.next(true);
                cmds.extend(dispatch_session(&outcome.coord, &outcome.ctx, first, 1));
                if cmds.len() < outcome.ctx.config.prefetch_budget {
                    cmds.extend(dispatch_session(
                        &outcome.coord,
                        &outcome.ctx,
                        first.other(),
                        1,
                    ));
                }
                if cmds.is_empty() {
                    return Poll::Pending;
                }
                return Poll::Ready(Some(cmds));
            }
            self.session_turns.reset();
            let active_budget = if has_incoming && outcome.ctx.config.prefetch_budget > 1 {
                outcome.ctx.config.prefetch_budget - 1
            } else {
                outcome.ctx.config.prefetch_budget
            };
            cmds.extend(outcome.coord.dispatch_active(&outcome.ctx, active_budget));
            let mut remaining = outcome
                .ctx
                .config
                .prefetch_budget
                .saturating_sub(cmds.len());
            if has_incoming && remaining > 0 {
                cmds.extend(outcome.coord.dispatch_incoming(&outcome.ctx, remaining));
                remaining = outcome
                    .ctx
                    .config
                    .prefetch_budget
                    .saturating_sub(cmds.len());
            }
            if remaining > 0 {
                cmds.extend(outcome.coord.dispatch_active(&outcome.ctx, remaining));
            }
        }
        if cmds.is_empty() {
            tracing::trace!(
                has_incoming = outcome.coord.has_incoming(),
                budget = outcome.ctx.config.prefetch_budget,
                "hls peer parked without commands"
            );
            return Poll::Pending;
        }
        Poll::Ready(Some(cmds))
    }

    fn priority(&self) -> RequestPriority {
        if self.activity.is_playing() {
            RequestPriority::High
        } else {
            RequestPriority::Low
        }
    }
}

fn dispatch_session<S>(
    coord: &HlsCoord<S>,
    ctx: &PlanCtx<S>,
    slot: SessionSlot,
    budget: usize,
) -> Vec<FetchCmd>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    match slot {
        SessionSlot::Active => coord.dispatch_active(ctx, budget),
        SessionSlot::Incoming => coord.dispatch_incoming(ctx, budget),
    }
}

/// Outcome of [`HlsPeer::poll_state_phase`]. Discriminates the three
/// terminal possibilities the caller must distinguish:
/// `Pending` (pre-activation), `Ready(None)` (stopped/cancelled), and
/// the normal continuation with everything `poll_next`'s lock-free
/// tail needs to dispatch + broadcast evictions.
enum PollPhase<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    NotActivated,
    Terminated,
    Continue(Box<PollOutcome<S>>),
}

struct PollOutcome<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    coord: Arc<HlsCoord<S>>,
    ctx: PlanCtx<S>,
    evictions: Vec<ResourceKey>,
    seg_at_reader: u32,
}

impl<S> HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Acquire the per-peer state lock and drive the four state-mutating
    /// stages of one poll cycle (seek detection → ABR/seek lock sync →
    /// segment-boundary commit → eviction drain). The guard drops at
    /// the end of the function so dispatch + broadcast run lock-free in
    /// the caller.
    ///
    /// Reconciles the ABR escape flag under the guard as an atomic-only step; a rising edge wants a
    /// controller re-tick, which reads `peer.progress()` and re-locks state, so that is deferred
    /// until after the guard drops.
    fn poll_state_phase(&self, cx: &mut Context<'_>) -> PollPhase<S> {
        let mut guard = self.state.lock();
        let Some(state) = guard.as_mut() else {
            *self.pending_waker.lock() = Some(cx.waker().clone());
            return PollPhase::NotActivated;
        };
        state.waker = Some(cx.waker().clone());

        let coord = Arc::clone(&state.coord);
        if coord.cancel.is_cancelled() {
            return PollPhase::Terminated;
        }
        let ctx = state.plan_ctx();

        state.apply_seek_change(&coord, &ctx);
        let seg_at_reader = state.apply_boundary_crossing(&coord, &ctx);
        let needs_retick = coord.reconcile_escape(seg_at_reader);
        let evictions = state.drain_evictions();
        drop(guard);
        coord.sync_abr_lock();
        if needs_retick {
            coord.abr.reevaluate();
        }

        PollPhase::Continue(Box::new(PollOutcome {
            coord,
            ctx,
            evictions,
            seg_at_reader,
        }))
    }
}

impl<S> HlsTrackState<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Resolve the reader's current segment from `coord.position()` and
    /// drive any pending ABR commit. The persistent variant queue (filled
    /// once by `rebuild`) advances the prefetch tail automatically as
    /// `dispatch` pops, so a boundary crossing alone does not need to
    /// refill the queue.
    ///
    /// Variant switching itself is not decided here: exact sessions publish
    /// their own commit once the incoming reader is ready, so this poll only
    /// tracks the reader's segment and re-aims the fetch plan when the byte
    /// space under the cursor was re-keyed.
    fn apply_boundary_crossing(&mut self, coord: &HlsCoord<S>, ctx: &PlanCtx<S>) -> u32 {
        let pos = coord.position();
        let prev = self.reader_segment.load(Ordering::Acquire);
        let variant_now = coord.variant_index();
        let variant_changed = self.reader_variant != variant_now;
        self.reader_variant = variant_now;
        let demand_segment = coord.demand_segment_at_offset(pos);
        let resolved = demand_segment.unwrap_or_else(|| u32::try_from(prev).unwrap_or(0));
        if let Some(floor) = self.seek_settle_floor {
            if demand_segment.is_some_and(|idx| idx >= floor) {
                self.seek_settle_floor = None;
            } else if let Some(landing) = demand_segment
                && floor.saturating_sub(landing) == 1
            {
                coord.active().rebuild(ctx, landing);
                self.seek_settle_floor = Some(landing);
                return landing;
            } else {
                return floor;
            }
        }
        let resolved_us = resolved as usize;
        let boundary_crossed = prev != resolved_us;
        if boundary_crossed {
            self.reader_segment.store(resolved_us, Ordering::Release);
        }
        let prev_u32 = u32::try_from(prev).unwrap_or(0);
        let discontinuous_advance = boundary_crossed && resolved != prev_u32.saturating_add(1);
        let aligned_rescue =
            variant_changed && demand_segment.is_some() && coord.active().served_from() == 0;
        if aligned_rescue {
            coord.active().rebuild_with_decoder_probe(ctx, resolved);
        } else if discontinuous_advance {
            coord.active().rebuild(ctx, resolved);
        }
        resolved
    }

    fn apply_seek_change(&mut self, coord: &HlsCoord<S>, ctx: &PlanCtx<S>) {
        let cur_seek = self.seek_obs.epoch();
        if cur_seek == self.last_seek_epoch {
            return;
        }
        self.last_seek_epoch = cur_seek;
        kithara::probe_event!(
            seek_epoch_reset,
            seek_epoch = cur_seek,
            segment_index = self.reader_segment.load(Ordering::Acquire),
            variant = coord.variant_index()
        );
        if let Some(target) = self.seek_obs.target()
            && let Some(seg) = coord.active().rebuild_at_time(ctx, target)
        {
            self.reader_segment.store(seg as usize, Ordering::Release);
            self.reader_variant = coord.variant_index();
            self.seek_settle_floor = Some(seg);
        }
    }

    /// Drain the eviction channel into a local buffer so the broadcast
    /// can run after the state lock drops.
    fn drain_evictions(&mut self) -> Vec<ResourceKey> {
        let mut out: Vec<ResourceKey> = Vec::new();
        while let Ok(key) = self.eviction_rx.try_recv() {
            out.push(key);
        }
        out
    }

    fn plan_ctx(&self) -> PlanCtx<S> {
        PlanCtx {
            bus: self.coord.emit.bus().clone(),
            scope: self.coord.scope.clone(),
            headers: self.coord.headers.clone(),
            config: self.config,
            seek_epoch: self.seek_obs.epoch(),
            signal: self.coord.signal(),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_stream::SeekState;

    use super::*;

    #[kithara::test]
    fn abr_cancel_observes_the_hls_track_scope() {
        let track_cancel = CancelToken::never();
        let seek = Arc::new(SeekState::new());
        let peer: HlsPeer<crate::test_pools::TestPools> = HlsPeer::new(
            Arc::clone(&seek) as Arc<dyn SeekObserve>,
            seek as Arc<dyn Activity>,
            AbrMode::default(),
            track_cancel.clone(),
        );
        let observed = Abr::cancel(&peer);

        assert!(!observed.is_cancelled());
        track_cancel.cancel();
        assert!(observed.is_cancelled());
    }

    #[kithara::test]
    fn one_slot_scheduler_alternates_active_and_incoming() {
        let turns = SessionTurns::default();

        assert_eq!(turns.next(true), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Incoming);
        assert_eq!(turns.next(true), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Incoming);
    }

    #[kithara::test]
    fn one_slot_scheduler_resets_when_no_incoming_session_exists() {
        let turns = SessionTurns::default();
        assert_eq!(turns.next(true), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Incoming);

        assert_eq!(turns.next(false), SessionSlot::Active);
        assert_eq!(turns.next(true), SessionSlot::Active);
    }
}
