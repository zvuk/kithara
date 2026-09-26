use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::Ordering,
};

use firewheel::{
    StreamInfo,
    node::{
        AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStore, ProcStreamCtx,
        ProcessStatus,
    },
    param::smoother::SmootherConfig,
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use kithara_sync::SyncExecutionReject;
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use num_traits::cast::AsPrimitive;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Observer, Producer},
};
use smallvec::SmallVec;

use super::{
    context::read_render_context,
    track::{PlayerTrack, SyncFadeTail},
};
use crate::{
    bridge::{
        NodeInputs, PlaybackShared, PlayerCmd, PlayerNotification, SyncReceiptTx, TrackState,
        TrackTransition,
        sync::{SyncReturn, SyncTicket},
    },
    rt::{RenderPass, RenderTargets, TrackSlot, TrackSlots, render::SyncRender},
    session::SessionError,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ContextRequirement {
    #[default]
    Standalone,
    Session,
}

/// The realtime audio processor for the player node.
///
/// Owns the loaded tracks, handles transitions, and renders mixed stereo audio into the Firewheel
/// output buffers.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerNodeProcessor {
    #[field(get, deref = false)]
    pub(super) playback: Arc<PlaybackShared>,
    pub(super) crossfade: crate::CrossfadeSettings,
    pub(super) cmd_rx: HeapCons<PlayerCmd>,
    pub(super) notif_tx: HeapProd<PlayerNotification>,
    pub(super) sample_rate: NonZeroU32,
    pub(super) render: RenderPass,
    pub(super) tracks: TrackSlots<{ Self::MAX_TRACKS }>,
    pub(super) tracks_transitions: VecDeque<TrackTransition>,
    pub(super) prefetch_duration: f32,
    context_requirement: ContextRequirement,
    trash_tx: HeapProd<PlayerTrack>,
    /// The sole producer stays alive until the callback processor is dropped.
    sync_receipts: Option<SyncReceiptTx>,
    sync_rx: HeapCons<SyncTicket>,
    sync_return_tx: HeapProd<SyncReturn>,
    sync_tail: Option<SyncFadeTail>,
    sync_return_held: Option<SyncReturn>,
    /// Last effective rate successfully delivered to the control thread.
    last_notified_rate: f32,
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamShape {
    pub max_block_frames: NonZeroU32,
    pub sample_rate: NonZeroU32,
}

impl StreamShape {
    #[must_use]
    pub const fn new(max_block_frames: NonZeroU32, sample_rate: NonZeroU32) -> Self {
        Self {
            max_block_frames,
            sample_rate,
        }
    }

    /// Compute decoder buffer depths, enforcing an application deadline when supplied.
    ///
    /// # Errors
    /// Returns an error when the geometry overflows or exceeds the budget.
    pub fn playback_buffers(
        self,
        quantum: NonZeroUsize,
        budget: Option<NonZeroUsize>,
    ) -> Result<(NonZeroUsize, NonZeroUsize), SessionError> {
        let output_frames = usize::try_from(self.max_block_frames.get())
            .map_err(|_| SessionError::ResponseGeometryOverflow)?;
        let preload = output_frames.div_ceil(quantum.get());
        let ring = preload
            .checked_add(1)
            .ok_or(SessionError::ResponseGeometryOverflow)?;
        let required_frames = ring
            .checked_add(1)
            .and_then(|chunks| chunks.checked_mul(quantum.get()))
            .and_then(|frames| frames.checked_sub(1))
            .ok_or(SessionError::ResponseGeometryOverflow)?;
        if let Some(budget) = budget
            && required_frames > budget.get()
        {
            return Err(SessionError::ResponseBudgetExceeded {
                required_frames,
                max_block_frames: self.max_block_frames.get(),
                render_quantum_frames: quantum.get(),
                budget_frames: budget.get(),
            });
        }
        Ok((
            NonZeroUsize::new(preload).ok_or(SessionError::ResponseGeometryOverflow)?,
            NonZeroUsize::new(ring).ok_or(SessionError::ResponseGeometryOverflow)?,
        ))
    }
}

impl PlayerNodeProcessor {
    /// Minimum position (seconds) before seeking is allowed on fade-in.
    pub(super) const FADE_IN_SEEK_THRESHOLD: f64 = 0.5;

    /// Maximum number of concurrent tracks per player node.
    pub const MAX_TRACKS: usize = 4;

    /// Create a new processor with the given command receiver and shared state.
    #[must_use]
    pub fn new<S>(
        inputs: NodeInputs,
        shape: StreamShape,
        pools: &PoolRegion<S>,
        gate_smoothing: SmootherConfig,
    ) -> Self
    where
        S: HasPool<f32>,
    {
        Self::with_context_requirement(
            inputs,
            shape,
            pools,
            gate_smoothing,
            ContextRequirement::Standalone,
        )
    }

    /// Clean up finished tracks, dropping `playing` once none is audible.
    ///
    /// When cleanup would empty the slot set after the queue plays out, keeps the track that
    /// reached natural EOF resident so an in-range seek can later revive it; `is_playing()` stays
    /// false until then.
    pub fn cleanup_finished_tracks(&mut self) {
        let finished: SmallVec<[(TrackSlot, bool); Self::MAX_TRACKS]> = self
            .tracks
            .iter()
            .filter(|(_, track)| track.state() == TrackState::Finished)
            .map(|(slot, track)| (slot, track.ended_at_eof()))
            .collect();

        let retain: Option<TrackSlot> = if finished.len() == self.tracks.len() {
            finished
                .iter()
                .find_map(|(slot, ended_at_eof)| ended_at_eof.then_some(*slot))
        } else {
            None
        };

        for (slot, _) in finished.iter().filter(|(slot, _)| Some(*slot) != retain) {
            if self
                .tracks
                .at_mut(*slot)
                .is_some_and(|track| track.has_sync_lane())
                && !self.can_return_sync()
            {
                continue;
            }
            if let Some(track) = self.tracks.remove_at(*slot) {
                let item_id = track.item_id();
                let src = Arc::clone(track.src());
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded { src, item_id })
                    .ok();
            }
        }

        if self.tracks.len() == 0 || retain.is_some() {
            self.playback.playing.store(false, Ordering::SeqCst);
        }
    }

    pub(super) fn discard_track(&mut self, track: PlayerTrack) {
        if track.sync_map().is_some_and(|map| {
            self.playback.active_sync_map.load(Ordering::Relaxed) == u64::from(map)
        }) {
            self.playback.active_sync_map.store(0, Ordering::Release);
        }
        if track.has_sync_lane() {
            self.return_sync(SyncReturn::Track(track));
            return;
        }
        if self.trash_tx.try_push(track).is_err() {
            self.playback.metrics().record_trash_overflow();
        }
    }

    pub(super) fn can_return_sync(&self) -> bool {
        self.sync_return_tx.vacant_len() > 0 || self.sync_return_held.is_none()
    }

    pub(super) fn sync_custody_cleared(&mut self) -> bool {
        self.sync_tail.is_none()
            && self.sync_rx.try_peek().is_none()
            && self.sync_return_held.is_none()
    }

    fn return_sync(&mut self, returned: SyncReturn) {
        if let Err(returned) = self.sync_return_tx.try_push(returned) {
            assert!(
                self.sync_return_held.is_none(),
                "one deck exceeded bounded sync return custody"
            );
            self.sync_return_held = Some(returned);
        }
    }

    fn maintain_sync_mailboxes(&mut self) {
        if let Some(returned) = self.sync_return_held.take()
            && let Err(returned) = self.sync_return_tx.try_push(returned)
        {
            self.sync_return_held = Some(returned);
        }
        if self.sync_tail.as_ref().is_some_and(SyncFadeTail::settled)
            && self.can_return_sync()
            && let Some(tail) = self.sync_tail.take()
        {
            self.return_sync(SyncReturn::Tail(tail));
        }
    }

    pub(super) fn retire_pending_sync(&mut self, reason: SyncExecutionReject) {
        if !self.can_return_sync() {
            return;
        }
        let Some(ticket) = self.sync_rx.try_peek() else {
            return;
        };
        if ticket.gate.still_permits(&ticket.permit) {
            let Some(receipts) = self.sync_receipts.as_mut() else {
                return;
            };
            if !receipts.publish_rejected(ticket.permit.stamp(), reason) {
                return;
            }
        }
        let Some(ticket) = self.sync_rx.try_pop() else {
            unreachable!("sole sync consumer lost a peeked ticket");
        };
        self.return_sync(SyncReturn::Ticket(ticket));
    }

    pub(super) fn retire_sync_tail(&mut self) {
        if self.can_return_sync()
            && let Some(tail) = self.sync_tail.take()
        {
            self.return_sync(SyncReturn::Tail(tail));
        }
    }

    pub(super) fn evict_tracks_if_needed(&mut self) {
        while self.tracks.is_full() {
            let Some((slot, state)) = self
                .tracks
                .iter()
                .filter(|(_, track)| !track.has_sync_lane() || self.can_return_sync())
                .min_by_key(|(_, track)| super::render::eviction_priority(track.state()))
                .map(|(slot, track)| (slot, track.state()))
            else {
                break;
            };

            if state == TrackState::Playing {
                self.playback.metrics().record_evicted_playing();
            }
            if let Some(track) = self.tracks.remove_at(slot) {
                let item_id = track.item_id();
                let src = Arc::clone(track.src());
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded { src, item_id })
                    .ok();
            }
        }
    }

    fn leading_effective_rate(&self) -> Option<f32> {
        self.tracks
            .iter()
            .find_map(|(_, track)| track.state().is_leading().then(|| track.playback_rate()))
    }

    fn publish_effective_rate(&mut self, rate: f32) {
        self.playback.rate.store(rate, Ordering::Relaxed);
        if self.last_notified_rate != rate
            && self
                .notif_tx
                .try_push(PlayerNotification::RateChanged { rate })
                .is_ok()
        {
            self.last_notified_rate = rate;
        }
    }

    pub(super) fn refresh_effective_rate(&mut self) {
        let rate = if self.playback.playing.load(Ordering::SeqCst) {
            self.leading_effective_rate().unwrap_or(0.0)
        } else {
            0.0
        };
        self.publish_effective_rate(rate);
    }

    pub fn render_audio(
        &mut self,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        self.render_with_context(None, buffers, frames, is_playing)
    }

    fn render_context<'a>(
        &self,
        store: &'a ProcStore,
        info: &ProcInfo,
    ) -> Result<Option<&'a RenderContext>, &'static str> {
        match self.context_requirement {
            ContextRequirement::Standalone => Ok(None),
            ContextRequirement::Session => read_render_context(store, info).map(Some),
        }
    }

    fn render_with_context(
        &mut self,
        context: Option<&RenderContext>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        self.render.render_audio(
            context,
            RenderTargets {
                tracks: &mut self.tracks,
                notification_tx: &mut self.notif_tx,
                metrics: self.playback.metrics(),
                seek_epoch: self.playback.seek_epoch.load(Ordering::SeqCst),
                sync: SyncRender {
                    pending: &mut self.sync_rx,
                    tail: &mut self.sync_tail,
                    receipts: &mut self.sync_receipts,
                    returns: &mut self.sync_return_tx,
                    playback: &self.playback,
                },
            },
            buffers,
            frames,
            is_playing,
        )
    }

    fn retire(&mut self, track: PlayerTrack) {
        let item_id = track.item_id();
        let src = Arc::clone(track.src());
        self.discard_track(track);
        self.notif_tx
            .try_push(PlayerNotification::Unloaded { src, item_id })
            .ok();
    }

    fn set_tracks_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.tracks
            .iter_mut()
            .for_each(|(_, track)| track.set_host_sample_rate(sample_rate));
    }

    pub(super) fn unload_slot(&mut self, slot: TrackSlot) {
        if self
            .tracks
            .at_mut(slot)
            .is_some_and(|track| track.has_sync_lane())
            && !self.can_return_sync()
        {
            return;
        }
        if let Some(track) = self.tracks.remove_at(slot) {
            self.retire(track);
        }
    }

    fn update_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        let rate_changed = self.sample_rate != sample_rate;
        self.sample_rate = sample_rate;
        self.playback
            .sample_rate
            .store(sample_rate.get(), Ordering::Relaxed);
        if rate_changed {
            self.set_tracks_host_sample_rate(sample_rate);
            self.render.update_sample_rate(sample_rate);
        }
    }

    /// Update `playback.position` / `playback.duration` from the
    /// leading track's last [`TrackReadOutcome`].
    ///
    /// `render_audio` captures the snapshot directly out of the outcome
    /// returned by `PlayerTrack::read`.
    /// Falls back to `track.position()` / `track.duration()` only when no
    /// leading track produced an outcome this cycle (cold start before
    /// the first render block, or every active track was a non-leading
    /// fade-in).
    ///
    /// Both published windows come from the leading track's lock-free snapshots: the decoded
    /// frontier, which is always `>=` position, and the cached span the download side published.
    fn update_position_duration(&self, leading_outcome: Option<(f64, f64)>) {
        for (_, track) in self.tracks.iter() {
            if track.state().is_leading() {
                self.playback
                    .frontier
                    .store(track.decoded_frontier(), Ordering::Relaxed);
                self.playback
                    .cached
                    .store(track.cached_span(), Ordering::Relaxed);
                break;
            }
        }

        if let Some((position, duration)) = leading_outcome {
            self.playback.position.store(position, Ordering::Relaxed);
            self.playback.duration.store(duration, Ordering::Relaxed);
            return;
        }

        for (_, track) in self.tracks.iter() {
            if track.state().is_leading() {
                self.playback
                    .position
                    .store(track.position(), Ordering::Relaxed);
                self.playback
                    .duration
                    .store(track.duration(), Ordering::Relaxed);
                break;
            }
        }
    }

    pub(super) fn with_context_requirement<S>(
        inputs: NodeInputs,
        shape: StreamShape,
        pools: &PoolRegion<S>,
        gate_smoothing: SmootherConfig,
        context_requirement: ContextRequirement,
    ) -> Self
    where
        S: HasPool<f32>,
    {
        let last_notified_rate = inputs.playback.rate.load(Ordering::Relaxed);
        Self {
            last_notified_rate,
            context_requirement,
            cmd_rx: inputs.cmd_rx,
            notif_tx: inputs.notif_tx,
            trash_tx: inputs.trash_tx,
            sync_receipts: inputs.sync_receipts,
            sync_rx: inputs.sync_rx,
            sync_return_tx: inputs.sync_return_tx,
            sync_tail: None,
            sync_return_held: None,
            playback: inputs.playback,
            sample_rate: shape.sample_rate,
            render: RenderPass::new(pools, shape, gate_smoothing),
            crossfade: crate::CrossfadeSettings::default(),
            prefetch_duration: 0.0,
            tracks: TrackSlots::default(),
            tracks_transitions: VecDeque::with_capacity(Self::MAX_TRACKS),
        }
    }

    delegate::delegate! {
        to self.tracks {
            /// Look up a track by its queue-item identity.
            #[must_use]
            #[call(get)]
            pub fn track(&self, item_id: TrackId) -> Option<&PlayerTrack>;
            /// Number of tracks currently held in the processor arena.
            #[must_use]
            #[call(len)]
            pub fn track_count(&self) -> usize;
            /// Look up a track by its queue-item identity (mutable).
            #[call(get_mut)]
            pub fn track_mut(&mut self, item_id: TrackId) -> Option<&mut PlayerTrack>;
        }
    }
}

impl AudioNodeProcessor for PlayerNodeProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.update_host_sample_rate(stream_info.sample_rate);
        self.render.resize(stream_info.max_block_frames.get().as_());
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        mut buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        self.playback.process_count.fetch_add(1, Ordering::Relaxed);

        self.drain_commands();

        self.maintain_sync_mailboxes();

        self.cleanup_finished_tracks();

        let is_playing = self.playback.playing.load(Ordering::SeqCst);

        let context = match self.render_context(&extra.store, info) {
            Ok(context) => context,
            Err(reason) => {
                let _ = extra.logger.try_error(reason);
                return ProcessStatus::ClearAllOutputs;
            }
        };

        let (playback_started, leading_outcome_pos_dur) =
            self.render_with_context(context, &mut buffers, info.frames, is_playing);

        self.update_position_duration(leading_outcome_pos_dur);
        self.refresh_effective_rate();

        if playback_started {
            ProcessStatus::OutputsModified
        } else {
            ProcessStatus::ClearAllOutputs
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use firewheel::{
        clock::InstantSamples,
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{ProcStore, StreamStatus},
    };
    use kithara_audio::mock::{AudioControlMock, AudioReadMock, AudioSessionMock};
    use kithara_events::EventBus;
    use kithara_platform::time::Duration;
    use kithara_signal::{
        AudioSpec, OutputContext, SessionEpoch, SessionFrame, SourceSpan, TransportRevision,
    };
    use kithara_sync::{LoadGeneration, PermitCell, SyncArbiter, SyncGateBinding, SyncReceipt};
    use kithara_warp::{BeatGridId, RenderContext, WarpMapRevision};
    use ringbuf::traits::{Consumer, Producer};
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        bridge::{
            SharedEq, slot_channels,
            sync::{PreparedFirst, sync_receipts},
        },
        resource::Resource,
        rt::sync_owner_fixture::prepared_entry,
        test_pools::pools,
    };

    #[kithara::test]
    fn application_deadline_is_optional_but_explicit_geometry_is_enforced() {
        let shape = StreamShape::new(
            NonZeroU32::new(512).expect("fixture block"),
            NonZeroU32::new(48_000).expect("fixture rate"),
        );
        let quantum = NonZeroUsize::new(32).expect("fixture quantum");
        let (preload, ring) = shape
            .playback_buffers(quantum, None)
            .expect("unbounded deadline");
        assert_eq!((preload.get(), ring.get()), (16, 17));
        assert!(matches!(
            shape.playback_buffers(quantum, NonZeroUsize::new(448)),
            Err(SessionError::ResponseBudgetExceeded {
                required_frames: 575,
                max_block_frames: 512,
                render_quantum_frames: 32,
                budget_frames: 448,
            })
        ));
    }

    fn processor() -> (PlayerNodeProcessor, crate::bridge::SlotControl) {
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let shape = StreamShape {
            sample_rate: NonZeroU32::new(44_100).expect("static sample rate"),
            max_block_frames: NonZeroU32::new(512).expect("static block size"),
        };
        (
            PlayerNodeProcessor::new(inputs, shape, &pools(), crate::DEFAULT_GATE_SMOOTHING),
            control,
        )
    }

    fn sync_resource(track: bool) -> Box<super::super::track::PlayerResource> {
        let rate = NonZeroU32::new(44_100).expect("fixture rate");
        let event_bus = AudioSessionMock::event_bus
            .each_call(matching!())
            .answers(&|mock| mock.make_ref(EventBus::new(1)));
        let spec = AudioReadMock::spec
            .each_call(matching!())
            .returns(AudioSpec::new(2, rate));
        let preload = AudioControlMock::preload
            .next_call(matching!())
            .returns(Ok(()));
        let reader = if track {
            let duration = AudioSessionMock::duration
                .each_call(matching!())
                .returns(Some(Duration::from_secs(1)));
            Unimock::new((event_bus, duration, spec, preload))
        } else {
            Unimock::new((event_bus, spec, preload))
        };
        let src: Arc<str> = Arc::from("clear-fixture");
        let resource = Resource::from_reader(reader, Some(Arc::clone(&src)));
        Box::new(
            super::super::track::PlayerResource::new(resource, src, &pools())
                .expect("fixture resource fits pool"),
        )
    }

    fn sync_track_and_tail(item_id: TrackId) -> (PlayerTrack, SyncFadeTail) {
        let rate = NonZeroU32::new(44_100).expect("fixture rate");
        let mut track = PlayerTrack::builder()
            .sample_rate(rate)
            .item_id(item_id)
            .load(LoadGeneration::first())
            .build(sync_resource(true));
        track.play();
        let tail = track.activate_sync(
            sync_resource(false),
            SourceSpan::new(0, 1, rate, 1).expect("source span"),
            WarpMapRevision::first(),
            rate,
            crate::CrossfadeSettings::default(),
        );
        (track, tail)
    }

    fn pending_ticket(item_id: TrackId) -> SyncTicket {
        let rate = NonZeroU32::new(44_100).expect("fixture rate");
        let member = BeatGridId::allocate().expect("member id");
        let group = BeatGridId::allocate().expect("group id");
        let (stamp, map) = prepared_entry(
            member,
            group,
            LoadGeneration::first(),
            TransportRevision::first(),
            Some(TransportRevision::first()),
            rate,
        );
        let arbiter = Arc::new(SyncArbiter::new());
        let cell = Arc::new(PermitCell::new(member));
        let owner = arbiter.try_control().expect("owner phase");
        let permit = owner.mint_permit(&cell, stamp).expect("exact permit");
        drop(owner);
        SyncTicket {
            item_id,
            load: LoadGeneration::first(),
            resource: sync_resource(false),
            first: PreparedFirst {
                stereo: [0.0, 0.0],
                source: SourceSpan::new(0, 1, rate, 1)
                    .expect("source span")
                    .with_mapping_revision(Some(NonZeroU64::MIN)),
            },
            permit,
            gate: SyncGateBinding::new(arbiter, cell),
            activation: SessionFrame::new(32),
            source_start: 0,
            epoch: SessionEpoch::new(1),
            output_rate: rate,
            map,
        }
    }

    #[kithara::test]
    fn pressured_clear_keeps_command_and_sync_custody_until_host_drains() {
        let (mut processor, mut control) = processor();
        let (receipt_tx, mut receipt_rx) = sync_receipts();
        processor.sync_receipts = Some(receipt_tx);
        let item_id = TrackId::allocate();
        let (active, first_tail) = sync_track_and_tail(item_id);
        assert!(processor.tracks.insert(active).is_none());
        let second_id = TrackId::allocate();
        let held_id = TrackId::allocate();
        let (_, second_tail) = sync_track_and_tail(second_id);
        let (_, held_tail) = sync_track_and_tail(held_id);
        assert!(
            processor
                .sync_return_tx
                .try_push(SyncReturn::Tail(first_tail))
                .is_ok()
        );
        assert!(
            processor
                .sync_return_tx
                .try_push(SyncReturn::Tail(second_tail))
                .is_ok()
        );
        processor.sync_return_held = Some(SyncReturn::Tail(held_tail));
        let pending = pending_ticket(item_id);
        let pending_stamp = pending.permit.stamp();
        assert!(control.sync_tx.try_push(pending).is_ok());
        assert!(control.cmd_tx.try_push(PlayerCmd::Clear).is_ok());
        assert!(
            control
                .cmd_tx
                .try_push(PlayerCmd::SetPrefetchDuration(0.25))
                .is_ok()
        );
        let mut returned = Vec::new();

        processor.drain_commands();
        assert_eq!(processor.tracks.len(), 1);
        assert!(processor.sync_rx.try_peek().is_some());
        assert!(matches!(
            processor.cmd_rx.try_peek(),
            Some(PlayerCmd::Clear)
        ));
        assert!(!processor.playback.playing.load(Ordering::SeqCst));
        assert_eq!(processor.prefetch_duration, 0.0);
        assert!(receipt_rx.try_pop().is_none());

        returned.push(control.sync_return_rx.try_pop().expect("first return"));
        processor.maintain_sync_mailboxes();
        processor.drain_commands();
        assert_eq!(processor.tracks.len(), 0);
        assert!(processor.sync_rx.try_peek().is_some());
        assert!(matches!(
            processor.sync_return_held.as_ref(),
            Some(SyncReturn::Track(_))
        ));
        assert!(matches!(
            processor.cmd_rx.try_peek(),
            Some(PlayerCmd::Clear)
        ));
        assert_eq!(processor.prefetch_duration, 0.0);

        returned.push(control.sync_return_rx.try_pop().expect("second return"));
        processor.maintain_sync_mailboxes();
        processor.drain_commands();
        assert!(processor.sync_rx.try_peek().is_none());
        assert!(matches!(
            processor.sync_return_held.as_ref(),
            Some(SyncReturn::Ticket(_))
        ));
        assert!(matches!(
            processor.cmd_rx.try_peek(),
            Some(PlayerCmd::Clear)
        ));
        assert_eq!(processor.prefetch_duration, 0.0);

        returned.push(control.sync_return_rx.try_pop().expect("third return"));
        processor.maintain_sync_mailboxes();
        processor.drain_commands();
        assert!(processor.sync_rx.try_peek().is_none());
        assert!(processor.sync_return_held.is_none());
        assert!(processor.cmd_rx.try_peek().is_none());
        assert!(!processor.playback.playing.load(Ordering::SeqCst));
        assert_eq!(processor.prefetch_duration, 0.25);
        assert!(matches!(receipt_rx.try_pop(), Some(SyncReceipt::Rejected {
            stamp,
            reason: SyncExecutionReject::Cancelled,
        }) if stamp == pending_stamp));
        assert!(receipt_rx.try_pop().is_none());

        while let Some(value) = control.sync_return_rx.try_pop() {
            returned.push(value);
        }
        assert_eq!(returned.len(), 5);
        let mut tails = Vec::new();
        let mut tracks = Vec::new();
        let mut tickets = Vec::new();
        for value in &returned {
            match value {
                SyncReturn::Tail(tail) => tails.push(tail.item_id),
                SyncReturn::Track(track) => tracks.push(track.item_id()),
                SyncReturn::Ticket(ticket) => tickets.push(ticket.permit.stamp()),
            }
        }
        assert_eq!(tails.len(), 3);
        for id in [item_id, second_id, held_id] {
            assert_eq!(
                tails
                    .iter()
                    .filter(|&&returned_id| returned_id == id)
                    .count(),
                1
            );
        }
        assert_eq!(tracks.as_slice(), &[item_id]);
        assert_eq!(tickets.as_slice(), &[pending_stamp]);

        let withdrawn = pending_ticket(item_id);
        let withdrawn_stamp = withdrawn.permit.stamp();
        let owner = withdrawn.gate.arbiter().try_control().expect("owner phase");
        owner
            .preflight_revoke(withdrawn.gate.cell())
            .expect("fixture permit revision")
            .revoke();
        drop(owner);
        assert!(control.sync_tx.try_push(withdrawn).is_ok());
        assert!(control.cmd_tx.try_push(PlayerCmd::Clear).is_ok());
        processor.drain_commands();
        assert!(processor.sync_rx.try_peek().is_none());
        assert!(processor.cmd_rx.try_peek().is_none());
        assert!(
            receipt_rx.try_pop().is_none(),
            "withdrawal already belongs to the owner"
        );
        assert!(matches!(
            control.sync_return_rx.try_pop(),
            Some(SyncReturn::Ticket(ticket)) if ticket.permit.stamp() == withdrawn_stamp
        ));
    }

    fn session_processor() -> PlayerNodeProcessor {
        let (inputs, _control) = slot_channels(SharedEq::new(0));
        let shape = StreamShape {
            sample_rate: NonZeroU32::new(44_100).expect("static sample rate"),
            max_block_frames: NonZeroU32::new(512).expect("static block size"),
        };
        PlayerNodeProcessor::with_context_requirement(
            inputs,
            shape,
            &pools(),
            crate::DEFAULT_GATE_SMOOTHING,
            ContextRequirement::Session,
        )
    }

    fn proc_info() -> ProcInfo {
        ProcInfo {
            sample_rate: NonZeroU32::new(44_100).expect("static sample rate"),
            frames: 512,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            total_cpu_seconds_recip: 1.0,
            process_to_playback_delay: None,
            did_just_unbypass: false,
            last_marker_instant: InstantSamples(0),
            sample_rate_recip: f64::from(44_100).recip(),
            clock_samples: InstantSamples(0),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        }
    }

    #[kithara::test]
    fn session_processors_read_the_same_host_context() {
        let mut store = ProcStore::with_capacity(1);
        super::super::install_render_context(&mut store)
            .expect("invariant: fixture installs one context slot");
        super::super::publish_render_context(
            &mut store,
            RenderContext::new_linear(
                OutputContext::new(
                    SessionFrame::new(0)..SessionFrame::new(512),
                    NonZeroU32::new(44_100).expect("static sample rate"),
                    SessionEpoch::new(3),
                    Some(TransportRevision::first()),
                )
                .expect("invariant: fixture output range is ordered"),
                None,
            )
            .expect("invariant: fixture context is valid"),
        )
        .expect("invariant: fixture context slot exists");
        let info = proc_info();
        let left = session_processor();
        let right = session_processor();
        let left = left
            .render_context(&store, &info)
            .expect("session context")
            .expect("required context");
        let right = right
            .render_context(&store, &info)
            .expect("session context")
            .expect("required context");

        assert!(std::ptr::eq(left, right));
        assert_eq!(left.output().session_epoch(), SessionEpoch::new(3));
        assert_eq!(
            left.output().transport_revision(),
            Some(TransportRevision::first())
        );
    }

    #[kithara::test]
    fn full_notification_ring_retries_latest_effective_rate_once() {
        let (mut processor, mut control) = processor();
        let filler = Arc::from("filler");
        while processor
            .notif_tx
            .try_push(PlayerNotification::Loaded {
                src: Arc::clone(&filler),
            })
            .is_ok()
        {}

        processor.publish_effective_rate(1.25);
        processor.publish_effective_rate(1.5);
        assert_eq!(processor.playback.rate.load(Ordering::Relaxed), 1.5);
        assert_eq!(processor.last_notified_rate, 0.0);

        assert!(control.notif_rx.try_pop().is_some());
        processor.publish_effective_rate(1.5);

        let mut delivered = Vec::new();
        while let Some(notification) = control.notif_rx.try_pop() {
            if let PlayerNotification::RateChanged { rate } = notification {
                delivered.push(rate);
            }
        }
        assert_eq!(delivered, [1.5]);

        processor.publish_effective_rate(1.5);
        assert!(control.notif_rx.try_pop().is_none());
    }
}
