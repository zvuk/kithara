#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use kithara_abr::{Abr, AbrState};
use kithara_assets::ResourceKey;
use kithara_events::{AbrMode, AbrProgressSnapshot, VariantDuration, VariantInfo};
use kithara_platform::{
    CancelToken,
    sync::Mutex,
    time::Duration,
    tokio::{self, sync::mpsc, task::spawn},
};
use kithara_stream::{
    Activity, DeferredWake, SeekObserve,
    dl::{FetchCmd, Peer, RequestPriority},
};
use kithara_test_utils::kithara;

use crate::{coord::HlsCoord, ids::duration_prefix, variant::PlanCtx};

struct HlsTrackState {
    coord: Arc<HlsCoord>,
    /// Narrow seek-observe handle — cloned from `HlsPeer.seek_obs` at
    /// activation time. All epoch/target/pending reads use this directly
    /// rather than routing through `coord.timeline`.
    seek_obs: Arc<dyn SeekObserve>,
    /// Reused from the parent [`HlsPeer`]: stores the reader's last-known
    /// segment index — read by [`Abr::progress`] and compared against the
    /// freshly resolved segment in `poll_next` to detect a boundary
    /// crossing. The initial value is set in [`HlsPeer::activate`].
    reader_segment: Arc<AtomicUsize>,
    /// Variant the stored `reader_segment` was resolved against. A
    /// variant switch re-keys the byte space under an unmoved cursor:
    /// the same segment index now points at a different variant's bytes,
    /// so `prev == resolved` no longer proves the reader stayed inside
    /// the planned window. [`HlsTrackState::apply_boundary_crossing`]
    /// treats a variant flip as a discontinuity and re-aims the fetch
    /// plan at the reader's actual segment.
    reader_variant: usize,
    /// Mirrors `HlsConfig::look_ahead_bytes` — capped idle prefetch
    /// budget threaded into every `PlanCtx` constructed for `dispatch`.
    look_ahead_bytes: Option<u64>,
    waker: Option<Waker>,
    eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
    last_seek_epoch: u64,
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
    prefetch_budget: usize,
}

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After [`activate`](Self::activate): each `poll_next` drains seek/ABR
/// commit/eviction events and asks the active [`HlsVariant`] for the
/// next batch of `FetchCmd`s (thin event router per spec).
pub(crate) struct HlsPeer {
    abr: Arc<AbrState>,
    /// Reader→peer wake channel. The HLS `Source` fires this whenever it
    /// advances the byte cursor or completes a seek, so `poll_next` runs
    /// again without waiting for the next downloader-driven wakeup. Owned
    /// here (not on `HlsCoord`) because the wake mechanism is a property
    /// of the peer, not of shared state.
    reader_advanced: Arc<DeferredWake>,
    reader_segment: Arc<AtomicUsize>,
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
    /// Narrow seek-observe handle. Used by `poll_next`'s inner logic
    /// (via `HlsTrackState`) to read the current epoch/target without
    /// holding a wide seek/playhead aggregate.
    seek_obs: Arc<dyn SeekObserve>,
    /// Narrow activity handle. Used by `priority()` to check whether
    /// the track is currently playing.
    activity: Arc<dyn Activity>,
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
    ) {
        let reader_advanced = Arc::clone(&self.reader_advanced);
        let cancel = coord.cancel.clone();

        let initial_seg = coord
            .find_at_offset(coord.position())
            .map_or(0, |(idx, _, _)| idx);
        if let Some(active) = coord.active() {
            let plan_ctx = PlanCtx {
                prefetch_budget,
                look_ahead_bytes,
                master_cancel: coord.cancel.clone(),
                scope: coord.scope.clone(),
                headers: coord.headers.clone(),
                seek_epoch: self.seek_obs.epoch(),
                ready: coord.ready_gate(),
                worker_wake: coord.worker_wake_cell(),
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
        // The ABR controller runs on the downloader runtime (off the RT produce
        // core), so wake the peer immediately rather than deferring.
        self.reader_advanced.notify_now();
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

        let cmds = outcome
            .coord
            .active()
            .map(|active| active.dispatch(&outcome.ctx, outcome.ctx.prefetch_budget))
            .unwrap_or_default();
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
        let evictions = state.drain_evictions();
        drop(guard);
        coord.sync_abr_lock();

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
        let found = coord.find_at_offset(pos);
        let resolved = found.map_or_else(|| u32::try_from(prev).unwrap_or(0), |(idx, _, _)| idx);
        // Forward-seek settle floor: while the reader's physical byte cursor
        // still lags behind a just-applied seek target, ignore this poll's
        // stale-low segment. `seek_epoch_reset` already aimed `reader_segment`
        // + the fetch plan at the target; without this guard the lagging
        // `resolved` re-keys the cursor backward and `rebuild`s the prefix
        // (re-downloading seg 0..target). Only drop the floor once the reader
        // *physically resolves* (`found.is_some()`) to a segment at/after it —
        // a `None` lookup falls back to `prev` and must NOT be read as "caught
        // up", or a later valid low lookup re-opens the race.
        if let Some(floor) = self.seek_settle_floor {
            if found.is_some_and(|(idx, _, _)| idx >= floor) {
                self.seek_settle_floor = None;
            } else {
                return floor;
            }
        }
        let resolved_us = resolved as usize;
        let boundary_crossed = prev != resolved_us;
        if boundary_crossed {
            self.reader_segment.store(resolved_us, Ordering::Release);
        }
        let manual_mode = matches!(self.coord.abr.mode(), Some(AbrMode::Manual(_)));
        let switch_landed = if boundary_crossed || manual_mode {
            coord.commit_variant_switch(ctx, resolved)
        } else if let Some(rescue_seg) = coord.urgent_rescue_boundary(resolved) {
            coord.commit_variant_switch(ctx, rescue_seg)
        } else {
            false
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
            && found.is_some()
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
        coord.reset_for_seek();
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
            ready: self.coord.ready_gate(),
            worker_wake: self.coord.worker_wake_cell(),
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
