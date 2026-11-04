#![forbid(unsafe_code)]

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use kithara_abr::{
    Abr, AbrMode, AbrProgressSnapshot, AbrState, VariantDuration, VariantIndex, VariantInfo,
};
use kithara_assets::ResourceKey;
use kithara_bufpool::HasPool;
use kithara_download::{FetchCmd, Peer, RequestPriority};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, Weak},
    time::Duration,
    tokio::sync::mpsc,
};
use kithara_stream::{Activity, DeferredWake, WorkerWake};
use kithara_test_utils::kithara;

use super::{
    scheduler::SessionTurns,
    track::{HlsTrackState, PollOutcome},
};
use crate::{ids::duration_prefix, stream::HlsCoord, variant::PlanCtx};

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

/// HLS peer — one per track. Pre-init: `poll_next` returns Pending.
/// After [`activate`](Self::activate): each `poll_next` drains seek/ABR
/// commit/eviction events and asks the active [`HlsVariant`] for the
/// next batch of `FetchCmd`s (thin event router per spec).
pub(crate) struct HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    abr: Arc<AbrState>,
    /// Narrow activity handle. Used by `priority()` to check whether
    /// the track is currently playing.
    activity: Activity,
    /// Reader→peer wake channel. The HLS `Source` fires this whenever it
    /// advances the byte cursor or completes a seek, so `poll_next` runs
    /// again without waiting for the next downloader-driven wakeup. Owned
    /// here (not on `HlsCoord`) because the wake mechanism is a property
    /// of the peer, not of shared state.
    reader_advanced: Arc<DeferredWake>,
    reader_segment: Arc<AtomicUsize>,
    state: Mutex<Option<HlsTrackState<S>>>,
    cancel: CancelToken,
    /// Resume a downloader poll made before track activation.
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
    pub(crate) fn new(activity: Activity, initial_mode: AbrMode, cancel: CancelToken) -> Self {
        let abr = Arc::new(AbrState::new(initial_mode));
        Self {
            activity,
            abr,
            cancel,
            state: Mutex::new(None),
            pending_waker: Mutex::default(),
            variants: Mutex::default(),
            reader_segment: Arc::new(AtomicUsize::new(0)),
            reader_advanced: Arc::new(DeferredWake::default()),
            session_turns: SessionTurns::default(),
        }
    }

    pub(crate) fn abr_state(&self) -> Arc<AbrState> {
        Arc::clone(&self.abr)
    }

    /// Activate track planning and forward reader notifications to the downloader.
    pub(crate) fn activate(
        self: &Arc<Self>,
        coord: Arc<HlsCoord<S>>,
        eviction_rx: mpsc::UnboundedReceiver<ResourceKey>,
    ) {
        let target: Arc<dyn WorkerWake> = Arc::new(PeerPollWake(Arc::downgrade(self)));
        self.reader_advanced.set_target(Arc::clone(&target));
        coord.set_peer_wake(Arc::clone(&self.reader_advanced), target);

        let initial_seg = coord
            .find_at_offset(coord.position())
            .map_or(0, |(idx, _, _)| idx);
        let active = coord.active();
        let plan_ctx = PlanCtx {
            config: Arc::clone(&coord.config),
            look_ahead_segments: coord.look_ahead_segments,
            bus: active.event_bus(),
            scope: coord.scope.clone(),
            signal: coord.signal(),
        };
        active.rebuild(&plan_ctx, initial_seg);
        self.reader_segment
            .store(initial_seg as usize, Ordering::Release);

        {
            let mut guard = self.state.lock();
            *guard = Some(HlsTrackState::new(
                coord,
                Arc::clone(&self.reader_segment),
                eviction_rx,
            ));
        }

        let pending_waker = self.pending_waker.lock().take();
        if let Some(waker) = pending_waker {
            waker.wake();
        }
    }

    /// Reader progress resumes `poll_next` through the peer's current off-core task waker.
    pub(crate) fn reader_wake(&self) -> Arc<DeferredWake> {
        Arc::clone(&self.reader_advanced)
    }

    pub(crate) fn set_abr_variants(&self, variants: Vec<VariantInfo>) {
        *self.variants.lock() = variants;
    }

    /// Release the coordinator and eviction receiver retained by the peer.
    pub(crate) fn teardown(&self) {
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
        let (current, download_head) = self.state.lock().as_ref().map_or_else(
            || (self.abr.current_variant_index(), 0),
            |state| {
                let session = state.coord.active_session();
                (
                    VariantIndex::new(session.variant_index()),
                    session.variant().download_head() as usize,
                )
            },
        );
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
    fn poll_next(&self, cx: &mut Context<'_>, budget: usize) -> Poll<Option<Vec<FetchCmd>>> {
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

        if budget == 0 {
            return Poll::Pending;
        }
        let cmds = self
            .session_turns
            .dispatch(&outcome.coord, &outcome.ctx, budget);
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
enum PollPhase<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    NotActivated,
    Terminated,
    Continue(Box<PollOutcome<S>>),
}

impl<S> HlsPeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// The track state reconciles while guarded; ABR reevaluation runs after
    /// the guard drops, since it reads peer progress.
    fn poll_state_phase(&self, cx: &mut Context<'_>) -> PollPhase<S> {
        let mut guard = self.state.lock();
        let Some(state) = guard.as_mut() else {
            *self.pending_waker.lock() = Some(cx.waker().clone());
            return PollPhase::NotActivated;
        };
        let Some(outcome) = state.reconcile(cx) else {
            return PollPhase::Terminated;
        };
        drop(guard);
        if outcome.needs_retick {
            outcome.coord.abr.reevaluate();
        }

        PollPhase::Continue(Box::new(outcome))
    }
}

#[cfg(test)]
mod tests {
    use kithara_stream::ActivityWriter;

    use super::*;

    #[kithara::test]
    fn abr_cancel_observes_the_hls_track_scope() {
        let track_cancel = CancelToken::never();
        let writer = ActivityWriter::new();
        let peer: HlsPeer<crate::test_pools::TestPools> =
            HlsPeer::new(writer.reader(), AbrMode::default(), track_cancel.clone());
        let observed = Abr::cancel(&peer);

        assert!(!observed.is_cancelled());
        track_cancel.cancel();
        assert!(observed.is_cancelled());
    }
}
