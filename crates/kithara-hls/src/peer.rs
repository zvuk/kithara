#![forbid(unsafe_code)]

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use kithara_abr::{Abr, AbrState};
use kithara_assets::ResourceKey;
use kithara_events::{AbrMode, AbrProgressSnapshot, VariantDuration, VariantInfo};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
    tokio::{self, sync::mpsc, task::spawn},
};
use kithara_stream::{
    Activity, DeferredWake, SeekObserve,
    dl::{FetchCmd, Peer, RequestPriority},
};
use kithara_test_utils::kithara;

use crate::{config::SizeProbeMethod, ids::duration_prefix, stream::HlsCoord, variant::PlanCtx};

struct HlsTrackState {
    coord: Arc<HlsCoord>,
    /// Reused from the parent [`HlsPeer`]: stores the reader's last-known
    /// segment index — read by [`Abr::progress`] and compared against the
    /// freshly resolved segment in `poll_next` to detect a boundary
    /// crossing. The initial value is set in [`HlsPeer::activate`].
    reader_segment: Arc<AtomicUsize>,
    /// Narrow seek-observe handle — cloned from `HlsPeer.seek_obs` at
    /// activation time. All epoch/target/pending reads use this directly
    /// rather than routing through `coord.timeline`.
    seek_obs: Arc<dyn SeekObserve>,
    /// Mirrors `HlsConfig::look_ahead_bytes` — capped idle prefetch
    /// budget threaded into every `PlanCtx` constructed for `dispatch`.
    look_ahead_bytes: Option<u64>,
    /// Effective media-segment cap used for small ephemeral stores.
    look_ahead_segments: Option<usize>,
    /// Target segment of an in-flight forward seek, held until the reader's
    /// physical byte cursor catches up to it. `coord.position()` only
    /// advances when the reader actually reads at the new offset, so right
    /// after a forward seek it still resolves to the *pre-seek* segment until
    /// the decoder recreate's first read lands (the off-RT `wait_range` is now
    /// event-driven, but a native-backend recreate can still take several
    /// scheduler polls). [`Self::seek_epoch_reset`] already aimed
    /// `reader_segment` + the fetch plan at this target;
    /// [`Self::apply_boundary_crossing`] honours it as a floor so a stale-low
    /// resolved segment cannot drag the cursor back and re-plan the prefix.
    /// Cleared once the reader physically resolves at/after the floor.
    seek_settle_floor: Option<u32>,
    waker: Option<Waker>,
    size_probe_method: SizeProbeMethod,
    eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
    last_seek_epoch: u64,
    prefetch_budget: usize,
    /// Variant the stored `reader_segment` was resolved against. A
    /// variant switch re-keys the byte space under an unmoved cursor:
    /// the same segment index now points at a different variant's bytes,
    /// so `prev == resolved` no longer proves the reader stayed inside
    /// the planned window. [`HlsTrackState::apply_boundary_crossing`]
    /// treats a variant flip as a discontinuity and re-aims the fetch
    /// plan at the reader's actual segment.
    reader_variant: usize,
}

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After [`activate`](Self::activate): each `poll_next` drains seek/ABR
/// commit/eviction events and asks the active [`HlsVariant`] for the
/// next batch of `FetchCmd`s (thin event router per spec).
pub(crate) struct HlsPeer {
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
    state: Arc<Mutex<Option<HlsTrackState>>>,
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
}

impl HlsPeer {
    pub(crate) fn new(
        seek_obs: Arc<dyn SeekObserve>,
        activity: Arc<dyn Activity>,
        initial_mode: AbrMode,
    ) -> Self {
        Self {
            seek_obs,
            activity,
            state: Arc::new(Mutex::new(None)),
            pending_waker: Mutex::default(),
            wake_signal: CancelToken::never(),
            abr: Arc::new(AbrState::new(initial_mode)),
            variants: Mutex::default(),
            reader_segment: Arc::new(AtomicUsize::new(0)),
            reader_advanced: Arc::new(DeferredWake::default()),
        }
    }

    pub(crate) fn activate(
        self: &Arc<Self>,
        coord: Arc<HlsCoord>,
        eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
        prefetch_budget: usize,
        look_ahead_bytes: Option<u64>,
        look_ahead_segments: Option<usize>,
        size_probe_method: SizeProbeMethod,
    ) {
        let reader_advanced = Arc::clone(&self.reader_advanced);
        // Let the `on_slow` hook wake this peer's `poll_next` when an in-flight
        // fetch stalls past `soft_timeout`, so `reconcile_escape` runs without
        // waiting for an incidental reader-progress wake.
        coord.set_peer_wake(Arc::clone(&self.reader_advanced));
        let cancel = coord.cancel.clone();

        let initial_seg = coord
            .find_at_offset(coord.position())
            .map_or(0, |(idx, _, _)| idx);
        if let Some(active) = coord.active() {
            let plan_ctx = PlanCtx {
                prefetch_budget,
                look_ahead_bytes,
                size_probe_method,
                look_ahead_segments,
                master_cancel: coord.cancel.clone(),
                scope: coord.scope.clone(),
                headers: coord.headers.clone(),
                seek_epoch: self.seek_obs.epoch(),
                signal: coord.signal(),
            };
            active.rebuild(&plan_ctx, initial_seg);
        }
        self.reader_segment
            .store(initial_seg as usize, Ordering::Release);

        {
            let mut guard = self.state.lock();
            *guard = Some(HlsTrackState {
                reader_variant: coord.variant_index(),
                coord,
                seek_obs: Arc::clone(&self.seek_obs),
                eviction_rx,
                prefetch_budget,
                look_ahead_bytes,
                look_ahead_segments,
                size_probe_method,
                last_seek_epoch: 0,
                seek_settle_floor: None,
                reader_segment: Arc::clone(&self.reader_segment),
                waker: None,
            });
        }

        if let Some(waker) = self.pending_waker.lock().take() {
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
                        let guard = peer.state.lock();
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

    fn commit_manual_switch_locked(state: Option<&mut HlsTrackState>) {
        let Some(state) = state else {
            return;
        };
        if !matches!(state.coord.abr.mode(), Some(AbrMode::Manual(_))) {
            return;
        }
        let coord = Arc::clone(&state.coord);
        if coord.cancel.is_cancelled() {
            return;
        }
        let ctx = state.plan_ctx();
        state.apply_seek_change(&coord, &ctx);
        state.apply_boundary_crossing(&coord, &ctx);
    }

    fn commit_manual_switch_now(&self) {
        Self::commit_manual_switch_locked(self.state.lock().as_mut());
    }

    /// Shared wake handle the `Source` clones to resume `poll_next` after a
    /// reader progress event. The reader drivers arm/notify it; this micro-task
    /// awaits [`DeferredWake::notified`].
    pub(crate) fn reader_wake(&self) -> Arc<DeferredWake> {
        Arc::clone(&self.reader_advanced)
    }

    pub(crate) fn set_abr_variants(&self, variants: Vec<VariantInfo>) {
        *self.variants.lock() = variants;
    }

    /// Release the stashed [`HlsTrackState`] and cancel the waker task so
    /// the peer drops its `Arc<HlsCoord>` (and the eviction receiver).
    pub(crate) fn teardown(&self) {
        self.wake_signal.cancel();
        let mut guard = self.state.lock();
        *guard = None;
    }
}

impl Drop for HlsPeer {
    fn drop(&mut self) {
        self.wake_signal.cancel();
    }
}

impl Abr for HlsPeer {
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
        // `reader_idx`/`download_head` are prefix endpoints into `durations`:
        // `idx == len` is the valid "at/after the last segment" endpoint and
        // sums the full slice; `idx > len` is impossible and surfaces as
        // `None` rather than being silently clamped.
        let reader_playback_time = duration_prefix(&durations, reader_idx)?;
        let download_head_playback_time = duration_prefix(&durations, download_head)?;
        Some(AbrProgressSnapshot {
            reader_playback_time,
            download_head_playback_time,
        })
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.abr))
    }

    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.lock().clone()
    }

    fn wake(&self) {
        self.commit_manual_switch_now();
        let waker = self
            .state
            .lock()
            .as_ref()
            .and_then(|state| state.waker.clone())
            .or_else(|| self.pending_waker.lock().clone());
        // The ABR controller is off the RT produce core: an explicit mode
        // change must re-poll the producer without waiting for reader motion.
        self.reader_advanced.notify_now();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Peer for HlsPeer {
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

        let mut cmds = outcome
            .coord
            .dispatch_pending_size_demands(&outcome.ctx, outcome.ctx.prefetch_budget);
        let remaining = outcome.ctx.prefetch_budget.saturating_sub(cmds.len());
        if remaining > 0 {
            cmds.extend(
                outcome
                    .coord
                    .active()
                    .map(|active| active.dispatch(&outcome.ctx, remaining))
                    .unwrap_or_default(),
            );
        }
        if cmds.is_empty() {
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

/// Outcome of [`HlsPeer::poll_state_phase`]. Discriminates the three
/// terminal possibilities the caller must distinguish:
/// `Pending` (pre-activation), `Ready(None)` (stopped/cancelled), and
/// the normal continuation with everything `poll_next`'s lock-free
/// tail needs to dispatch + broadcast evictions.
enum PollPhase {
    NotActivated,
    Terminated,
    Continue(Box<PollOutcome>),
}

struct PollOutcome {
    coord: Arc<HlsCoord>,
    ctx: PlanCtx,
    evictions: Vec<ResourceKey>,
    seg_at_reader: u32,
}

impl HlsPeer {
    /// Acquire the per-peer state lock and drive the four state-mutating
    /// stages of one poll cycle (seek detection → ABR/seek lock sync →
    /// segment-boundary commit → eviction drain). The guard drops at
    /// the end of the function so dispatch + broadcast run lock-free in
    /// the caller.
    fn poll_state_phase(&self, cx: &mut Context<'_>) -> PollPhase {
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
        // Reconcile the ABR escape flag under the guard (atomic-only). A rising
        // edge wants a controller re-tick, which reads `peer.progress()` and
        // re-locks `state` — so defer it until after `drop(guard)`.
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

impl HlsTrackState {
    /// Resolve the reader's current segment from `coord.position()` and
    /// drive any pending ABR commit. The persistent variant queue (filled
    /// once by `rebuild`) advances the prefetch tail automatically as
    /// `dispatch` pops, so a boundary crossing alone does not need to
    /// refill the queue.
    ///
    /// Decide when to call `commit_variant_switch`:
    ///
    /// - **Auto mode**: commit fires only on *actual* boundary crossings
    ///   (`prev != resolved`). Pending decisions from the bandwidth
    ///   controller wait for the reader to physically advance to the
    ///   next segment, so an aggressive in-segment up-switch does not
    ///   pin `v_new` prematurely.
    /// - **Manual mode**: commit fires on every poll. User click
    ///   `handle.set_mode(Manual(N))` is an explicit intent — wait for
    ///   a boundary cross that may never come (all-cached, idle peer)
    ///   makes the switch silently fail. The cross-variant byte
    ///   routing in [`HlsCoord::variant_serving`] + the no-forward-jump
    ///   guarantee in `activate_at_segment_with_shift` keep decoder
    ///   continuity intact even when commit lands mid-segment.
    fn apply_boundary_crossing(&mut self, coord: &HlsCoord, ctx: &PlanCtx) -> u32 {
        let pos = coord.position();
        let prev = self.reader_segment.load(Ordering::Acquire);
        let variant_now = coord.variant_index();
        let variant_changed = self.reader_variant != variant_now;
        self.reader_variant = variant_now;
        let demand_segment = coord.demand_segment_at_offset(pos);
        let resolved = demand_segment.unwrap_or_else(|| u32::try_from(prev).unwrap_or(0));
        // Forward-seek settle floor: while the reader's physical byte cursor
        // still lags behind a just-applied seek target, ignore this poll's
        // stale-low segment. `seek_epoch_reset` already aimed `reader_segment`
        // + the fetch plan at the target; without this guard the lagging
        // `resolved` re-keys the cursor backward and `rebuild`s the prefix
        // (re-downloading seg 0..target). Only drop the floor once the reader
        // *physically resolves* (`demand_segment.is_some()`) to a segment
        // a `None` lookup falls back to `prev` and must NOT be read as "caught
        // up", or a later valid low lookup re-opens the race.
        if let Some(floor) = self.seek_settle_floor {
            // A pending Manual switch is explicit user intent and must commit
            // on a seek poll REGARDLESS of which floor sub-case applies. Left
            // behind the floor it self-reinforces: the reader cannot resolve
            // at/after the floor while the switch is uncommitted (the scheduler
            // no longer fetches the old variant), and a coincident decoder
            // backoff landing would route through the re-aim arm below before
            // the switch ever commits. Commit it up-front, then keep the floor
            // for the re-key suppression. Auto never matches (no `Manual`), so
            // the seek-no-switch freeze contract is untouched; a single-variant
            // `Manual(N)` with no pending decision is a no-op.
            if matches!(self.coord.abr.mode(), Some(AbrMode::Manual(_))) {
                coord.commit_variant_switch_at_segment(ctx, floor);
            }
            if demand_segment.is_some_and(|idx| idx >= floor) {
                self.seek_settle_floor = None;
            } else if let Some(landing) = demand_segment
                && floor.saturating_sub(landing) == 1
            {
                // Decoder-backoff landing, NOT a lagging cursor. The seek
                // re-aimed the queue + floor at the time-derived target
                // (`seek_epoch_reset` → `rebuild_at_time`), but the codec's
                // recreate seek backs off by its warmup preroll and parks one
                // segment earlier (60s target → 54s landing). `set_position`
                // already put the reader cursor in that landing segment, yet
                // the queue starts at the target segment, so the landing
                // segment is never fetched and the decoder (reading forward
                // from the landing) starves. Re-aim the prefetch at the
                // reader's actual segment and drop the floor to it; the floor
                // then clears normally once the cursor resolves at/after the
                // target. The one-segment bound (`floor - landing == 1`) is
                // the codec preroll bound: a stale *lagging* cursor resolves
                // many segments below the floor and stays suppressed (the
                // prefix re-download guard the floor was built for is intact).
                if let Some(active) = coord.active() {
                    active.rebuild(ctx, landing);
                }
                self.seek_settle_floor = Some(landing);
                return landing;
            } else {
                // Lagging cursor: suppress the re-key/rebuild below (the
                // floor's sole job — guards rapid-seek prefix re-download).
                return floor;
            }
        }
        let resolved_us = resolved as usize;
        let boundary_crossed = prev != resolved_us;
        if boundary_crossed {
            self.reader_segment.store(resolved_us, Ordering::Release);
        }
        let manual_mode = matches!(self.coord.abr.mode(), Some(AbrMode::Manual(_)));
        let switch_landed = match coord.urgent_rescue_boundary(resolved) {
            Some(rescue_seg) => coord.commit_variant_switch(ctx, rescue_seg),
            // Reader pinned at a clean boundary on a non-delivering segment with
            // a pending switch — the current variant cannot cross the boundary,
            // so commit the target AT the stalled segment instead of waiting for
            // a reader advance that can never happen (startup/stall livelock).
            None if coord.stalled_escape(resolved) => {
                coord.commit_variant_switch_at_segment(ctx, resolved)
            }
            None if boundary_crossed || manual_mode => coord.commit_variant_switch(ctx, resolved),
            None => false,
        };
        let prev_u32 = u32::try_from(prev).unwrap_or(0);
        let discontinuous_advance = boundary_crossed && resolved != prev_u32.saturating_add(1);
        // A switch committed since the last poll re-keys the byte space
        // under the cursor: the commit planned from its time-derived
        // target segment, while the decoder's recreate seek backs off by
        // the codec warmup and can land one segment earlier — a segment
        // the plan excludes and that no seek-epoch reseed owns (a
        // FormatBoundary recreate never bumps the epoch). Re-aim the
        // plan at the reader's actual segment; the probe variant keeps
        // the init/header seeds a mid-flight recreate still needs, and
        // slot dedup makes a redundant reseed free. Two gates: never
        // reseed from the `prev` fallback (an old-variant-space index),
        // and skip the shifted byte-continuity path (`served_from != 0`
        // cannot host a FormatBoundary recreate — `header_byte_range`
        // is not applicable there — and the reader still reads the
        // pre-switch range from `v_old`).
        let aligned_rescue = variant_changed
            && !switch_landed
            && demand_segment.is_some()
            && coord.active().is_some_and(|a| a.served_from() == 0);
        if aligned_rescue {
            if let Some(active) = coord.active() {
                active.rebuild_with_decoder_probe(ctx, resolved);
            }
        } else if discontinuous_advance
            && !switch_landed
            && let Some(active) = coord.active()
        {
            active.rebuild(ctx, resolved);
        }
        resolved
    }

    /// Detect a seek-epoch bump on the shared seek state and delegate the
    /// reset work to [`Self::seek_epoch_reset`] (which carries the
    /// probe). Called every poll cycle; the equality short-circuit
    /// keeps it free in the steady state.
    ///
    /// Collapses cross-variant byte-continuity layering on the new
    /// seek epoch — a random seek into pre-switch territory would
    /// otherwise pull bytes from a `history` variant whose data does
    /// not match the variant ABR currently considers active.
    fn apply_seek_change(&mut self, coord: &HlsCoord, ctx: &PlanCtx) {
        let cur_seek = self.seek_obs.epoch();
        if cur_seek == self.last_seek_epoch {
            return;
        }
        self.last_seek_epoch = cur_seek;
        coord.prepare_for_seek();
        self.seek_epoch_reset(coord, ctx);
    }

    /// Drain the eviction channel into a local buffer so the broadcast
    /// can run after the state lock drops.
    fn drain_evictions(&mut self) -> Vec<ResourceKey> {
        let mut out = Vec::new();
        while let Ok(key) = self.eviction_rx.try_recv() {
            out.push(key);
        }
        out
    }

    fn plan_ctx(&self) -> PlanCtx {
        PlanCtx {
            master_cancel: self.coord.cancel.clone(),
            scope: self.coord.scope.clone(),
            headers: self.coord.headers.clone(),
            prefetch_budget: self.prefetch_budget,
            seek_epoch: self.seek_obs.epoch(),
            look_ahead_bytes: self.look_ahead_bytes,
            look_ahead_segments: self.look_ahead_segments,
            signal: self.coord.signal(),
            size_probe_method: self.size_probe_method,
        }
    }

    /// React to a confirmed seek-epoch bump: reposition the active
    /// variant at the new target time and sync `reader_segment`.
    /// Extracted from [`Self::apply_seek_change`] so the actual reset
    /// work owns a USDT probe (`kithara_hls_probe::seek_epoch_reset`)
    /// — integration tests key off this probe to detect scheduler
    /// epoch resets without polling cycles flooding the wire.
    #[kithara::probe(
        seek_epoch = self.seek_obs.epoch(),
        segment_index = self.reader_segment.load(Ordering::Acquire),
        variant = coord.variant_index()
    )]
    fn seek_epoch_reset(&mut self, coord: &HlsCoord, ctx: &PlanCtx) {
        if let Some(target) = self.seek_obs.target()
            && let Some(active) = coord.active()
            && let Some(seg) = active.rebuild_at_time(ctx, target)
        {
            self.reader_segment.store(seg as usize, Ordering::Release);
            self.reader_variant = coord.variant_index();
            self.seek_settle_floor = Some(seg);
        }
    }
}
