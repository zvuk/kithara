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
    Mutex,
    time::Duration,
    tokio::{
        self,
        sync::{Notify, mpsc},
    },
};
use kithara_stream::{
    Timeline,
    dl::{FetchCmd, Peer, RequestPriority},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;

use crate::{coord::HlsCoord, variant::PlanCtx};

struct HlsTrackState {
    coord: Arc<HlsCoord>,
    /// Reused from the parent [`HlsPeer`]: stores the reader's last-known
    /// segment index — read by [`Abr::progress`] and compared against the
    /// freshly resolved segment in `poll_next` to detect a boundary
    /// crossing. The initial value is set in [`HlsPeer::activate`].
    reader_segment: Arc<AtomicUsize>,
    /// Mirrors `HlsConfig::look_ahead_bytes` — capped idle prefetch
    /// budget threaded into every `PlanCtx` constructed for `dispatch`.
    look_ahead_bytes: Option<u64>,
    waker: Option<Waker>,
    eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
    last_seek_epoch: u64,
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
    reader_advanced: Arc<Notify>,
    reader_segment: Arc<AtomicUsize>,
    state: Arc<Mutex<Option<HlsTrackState>>>,
    /// Wake-up trigger for the waker-forwarding micro-task: not a
    /// cancellation of work — fires from `teardown()` / `Drop`.
    /// // kithara:cancel:owner
    wake_signal: CancellationToken,
    pending_waker: Mutex<Option<Waker>>,
    /// Single source of truth for variant metadata visible to ABR
    /// controller via [`Abr::variants()`] and to UI/FFI via the
    /// `Source::current_variant()` chain. Populated once by
    /// [`Self::set_abr_variants`] after the master + media playlists
    /// have been parsed; never mutated again for the peer's lifetime.
    variants: Mutex<Vec<VariantInfo>>,
    timeline: Timeline,
}

impl HlsPeer {
    pub(crate) fn new(timeline: Timeline, initial_mode: AbrMode) -> Self {
        Self {
            timeline,
            state: Arc::new(Mutex::new(None)),
            pending_waker: Mutex::new(None),
            wake_signal: CancellationToken::new(), // kithara:cancel:owner
            abr: Arc::new(AbrState::new(initial_mode)),
            variants: Mutex::new(Vec::new()),
            reader_segment: Arc::new(AtomicUsize::new(0)),
            reader_advanced: Arc::new(Notify::new()),
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
                asset_store: Arc::clone(&coord.asset_store),
                seek_epoch: coord.timeline.seek_epoch(),
            };
            active.rebuild(&plan_ctx, initial_seg);
        }
        self.reader_segment
            .store(initial_seg as usize, Ordering::Release);

        {
            let mut guard = self.state.lock_sync();
            *guard = Some(HlsTrackState {
                coord,
                eviction_rx,
                prefetch_budget,
                look_ahead_bytes,
                last_seek_epoch: 0,
                reader_segment: Arc::clone(&self.reader_segment),
                waker: None,
            });
        }

        if let Some(waker) = self.pending_waker.lock_sync().take() {
            waker.wake();
        }

        let peer_weak = Arc::downgrade(self);
        let wake_signal = self.wake_signal.clone();
        tokio::task::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = wake_signal.cancelled() => return,
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

    /// Shared `Notify` handle that the `Source` clones to wake `poll_next`
    /// after every reader progress event.
    pub(crate) fn reader_wake(&self) -> Arc<Notify> {
        Arc::clone(&self.reader_advanced)
    }

    pub(crate) fn set_abr_variants(&self, variants: Vec<VariantInfo>) {
        *self.variants.lock_sync() = variants;
    }

    /// Release the stashed [`HlsTrackState`] and cancel the waker task so
    /// the peer drops its `Arc<HlsCoord>` (and the eviction receiver).
    pub(crate) fn teardown(&self) {
        self.wake_signal.cancel();
        let mut guard = self.state.lock_sync();
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
            .lock_sync()
            .iter()
            .find(|v| v.variant_index == current)
            .and_then(|v| match &v.duration {
                VariantDuration::Segmented(d) => Some(d.clone()),
                VariantDuration::Total(_) | VariantDuration::Unknown => None,
            })?;
        let reader_idx = self.reader_segment.load(Ordering::Acquire);
        let download_head = self
            .state
            .lock_sync()
            .as_ref()
            .map_or(0, |s| s.coord.download_head() as usize);
        let reader_clamped = reader_idx.min(durations.len());
        let head_clamped = download_head.min(durations.len());
        let reader_playback_time: Duration = durations[..reader_clamped].iter().copied().sum();
        let download_head_playback_time: Duration = durations[..head_clamped].iter().copied().sum();
        Some(AbrProgressSnapshot {
            reader_playback_time,
            download_head_playback_time,
        })
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.abr))
    }

    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.lock_sync().clone()
    }

    fn wake(&self) {
        self.reader_advanced.notify_one();
    }
}

impl Peer for HlsPeer {
    #[kithara::probe]
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let outcome = match self.poll_state_phase(cx) {
            PollPhase::NotActivated => return Poll::Pending,
            PollPhase::Terminated => return Poll::Ready(None),
            PollPhase::Continue(o) => o,
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
        if self.timeline.is_playing() {
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
    Continue(PollOutcome),
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
        let mut guard = self.state.lock_sync();
        let Some(state) = guard.as_mut() else {
            *self.pending_waker.lock_sync() = Some(cx.waker().clone());
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

        PollPhase::Continue(PollOutcome {
            coord,
            ctx,
            evictions,
            seg_at_reader,
        })
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
        let resolved = coord
            .find_at_offset(pos)
            .map_or_else(|| u32::try_from(prev).unwrap_or(0), |(idx, _, _)| idx);
        let resolved_us = resolved as usize;
        let boundary_crossed = prev != resolved_us;
        if boundary_crossed {
            self.reader_segment.store(resolved_us, Ordering::Release);
        }
        let manual_mode = matches!(self.coord.abr.mode(), Some(AbrMode::Manual(_)));
        let switch_landed = if boundary_crossed || manual_mode {
            coord.commit_variant_switch(ctx, resolved)
        } else {
            false
        };
        let prev_u32 = u32::try_from(prev).unwrap_or(0);
        let discontinuous_advance = boundary_crossed && resolved != prev_u32.saturating_add(1);
        if discontinuous_advance
            && !switch_landed
            && let Some(active) = coord.active()
        {
            active.rebuild(ctx, resolved);
        }
        resolved
    }

    /// Detect a seek-epoch bump on the [`Timeline`] and delegate the
    /// reset work to [`Self::seek_epoch_reset`] (which carries the
    /// probe). Called every poll cycle; the equality short-circuit
    /// keeps it free in the steady state.
    ///
    /// Collapses cross-variant byte-continuity layering on the new
    /// seek epoch — a random seek into pre-switch territory would
    /// otherwise pull bytes from a `history` variant whose data does
    /// not match the variant ABR currently considers active.
    fn apply_seek_change(&mut self, coord: &HlsCoord, ctx: &PlanCtx) {
        let cur_seek = coord.timeline.seek_epoch();
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
            asset_store: Arc::clone(&self.coord.asset_store),
            prefetch_budget: self.prefetch_budget,
            seek_epoch: self.coord.timeline.seek_epoch(),
            look_ahead_bytes: self.look_ahead_bytes,
        }
    }

    /// React to a confirmed seek-epoch bump: reposition the active
    /// variant at the new target time and sync `reader_segment`.
    /// Extracted from [`Self::apply_seek_change`] so the actual reset
    /// work owns a USDT probe (`kithara_hls_probe::seek_epoch_reset`)
    /// — integration tests key off this probe to detect scheduler
    /// epoch resets without polling cycles flooding the wire.
    #[kithara::probe(
        seek_epoch = coord.timeline.seek_epoch(),
        segment_index = self.reader_segment.load(Ordering::Acquire),
        variant = coord.variant_index()
    )]
    fn seek_epoch_reset(&mut self, coord: &HlsCoord, ctx: &PlanCtx) {
        if let Some(target) = coord.timeline.seek_target()
            && let Some(active) = coord.active()
            && let Some(seg) = active.rebuild_at_time(ctx, target)
        {
            self.reader_segment.store(seg as usize, Ordering::Release);
        }
    }
}
