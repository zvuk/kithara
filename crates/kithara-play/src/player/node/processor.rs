use std::{collections::VecDeque, num::NonZeroU32, sync::atomic::Ordering};

use firewheel::{
    StreamInfo,
    dsp::fade::FadeCurve,
    event::ProcEvents,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_bufpool::PcmPool;
use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapCons, HeapProd, traits::Producer};
use thunderdome::Index;
use tracing::warn;

use super::{ArenaRegistry, RenderPass, RenderTargets};
use crate::{
    api::{SessionBeat, TransportRevision},
    bridge::{
        NodeInputs, PlaybackShared, PlayerCmd, PlayerNotification, SessionSeekAttempt, TrackState,
        TrackTransition,
    },
    player::track::{PlayerTrack, TrackRenderMode},
    session::render::{TransportBoundary, read_render_context},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PendingSessionSeek {
    pub(super) target: SessionBeat,
    pub(super) attempt: SessionSeekAttempt,
    pub(super) revision: TransportRevision,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum CrossfadeCurve {
    #[default]
    EqualPower,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ContextRequirement {
    #[default]
    Standalone,
    Session,
}

fn map_curve(curve: CrossfadeCurve) -> FadeCurve {
    match curve {
        CrossfadeCurve::EqualPower => FadeCurve::SquareRoot,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CrossfadeSettings {
    pub(crate) curve: CrossfadeCurve,
    pub(crate) duration: f32,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            curve: CrossfadeCurve::default(),
            duration: 1.0,
        }
    }
}

impl CrossfadeSettings {
    pub(crate) fn fade_curve(&self) -> FadeCurve {
        map_curve(self.curve)
    }
}

/// The realtime audio processor for the player node.
///
/// Manages tracks in a thunderdome arena, handles transitions,
/// and renders mixed stereo audio into the Firewheel output buffers.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct PlayerNodeProcessor {
    #[field(get, deref = false)]
    pub(super) playback: Arc<PlaybackShared>,
    pub(super) tracks: ArenaRegistry<Arc<str>, PlayerTrack>,
    pub(super) crossfade: CrossfadeSettings,
    pub(super) cmd_rx: HeapCons<PlayerCmd>,
    pub(super) notif_tx: HeapProd<PlayerNotification>,
    pub(super) sample_rate: NonZeroU32,
    pub(super) session_seek: Option<PendingSessionSeek>,
    pub(super) render: RenderPass,
    pub(super) tracks_transitions: VecDeque<TrackTransition>,
    pub(super) prefetch_duration: f32,
    context_requirement: ContextRequirement,
    trash_tx: HeapProd<PlayerTrack>,
    pending_retirement: Option<PlayerTrack>,
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct StreamShape {
    /// Maximum frames the graph may request in one render block.
    pub max_block_frames: NonZeroU32,
    /// Output sample rate observed by the render graph.
    pub sample_rate: NonZeroU32,
}

impl StreamShape {
    /// Creates a stream shape from validated non-zero dimensions.
    #[must_use]
    pub const fn new(sample_rate: NonZeroU32, max_block_frames: NonZeroU32) -> Self {
        Self {
            max_block_frames,
            sample_rate,
        }
    }
}

impl PlayerNodeProcessor {
    /// Minimum position (seconds) before seeking is allowed on fade-in.
    pub(super) const FADE_IN_SEEK_THRESHOLD: f64 = 0.5;

    /// Maximum number of concurrent tracks per player node.
    pub(super) const MAX_TRACKS: usize = 4;

    /// Create a new processor with the given command receiver and shared state.
    #[must_use]
    pub fn new(inputs: NodeInputs, shape: StreamShape, pool: &PcmPool) -> Self {
        Self::with_context_requirement(inputs, shape, pool, ContextRequirement::Standalone)
    }

    /// Clean up finished tracks.
    ///
    /// Uses a stack-allocated array instead of `Vec` since `Self::MAX_TRACKS` is 4,
    /// avoiding heap allocation on every `process()` call.
    ///
    /// When the cleanup empties the arena (last track played out to natural
    /// EOF), drop `state.playing` so `Player::is_playing()` reflects the
    /// stopped state without a separate `SetPaused` round-trip from the
    /// queue layer — the queue's `QueueEnded` path can leave the arena
    /// running so the tail samples drain instead of cutting them off.
    pub fn cleanup_finished_tracks(&mut self) {
        let mut finished: [Option<(Arc<str>, Index)>; Self::MAX_TRACKS] =
            [const { None }; Self::MAX_TRACKS];
        let mut count = 0;

        for (key, idx) in self
            .tracks
            .iter_keys()
            .filter(|(_, idx)| {
                self.tracks
                    .get_by_index(**idx)
                    .is_some_and(|track| track.state() == TrackState::Finished)
            })
            .take(Self::MAX_TRACKS)
        {
            finished[count] = Some((Arc::clone(key), *idx));
            count += 1;
        }

        // Superpowered-style end-of-queue resume: if removing the finished
        // tracks would empty the arena (the queue has played out) and one of
        // them reached *natural* EOF, keep that single track resident (warm)
        // so a later in-range seek can revive it (`apply_seek`). It is
        // reclaimed by `evict_tracks_if_needed` (Finished evicts first) when
        // the next track loads. Tracks that finished via `stop()` or a
        // faded-out crossfade (not `ended_at_eof`) are discarded as usual.
        let retain: Option<Index> = if count == self.tracks.len() {
            finished[..count].iter().flatten().find_map(|(_, idx)| {
                self.tracks
                    .get_by_index(*idx)
                    .filter(|t| t.ended_at_eof())
                    .map(|_| *idx)
            })
        } else {
            None
        };

        for entry in finished[..count].iter().flatten() {
            if self.retirement_blocked() {
                break;
            }
            let (key, idx) = entry;
            if Some(*idx) == retain {
                continue;
            }
            if let Some(track) = self.tracks.remove_by_index(*idx) {
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded {
                        src: Arc::clone(key),
                    })
                    .ok();
            }
        }

        // Playback has stopped once nothing audible remains: either the arena
        // is empty (all finished discarded) or only the retained, played-out
        // track is left. The retained track is inert in `render_audio` (gated
        // by `is_playing`), so `is_playing()` correctly stays false until a
        if self.tracks.len() == 0 || retain.is_some() {
            self.playback.playing.store(false, Ordering::SeqCst);
        }
    }

    /// Hand a retired track to the control thread without dropping it here.
    pub(super) fn discard_track(&mut self, track: PlayerTrack) {
        debug_assert!(self.pending_retirement.is_none());
        if let Err(track) = self.trash_tx.try_push(track) {
            self.pending_retirement = Some(track);
        }
    }

    /// Evict tracks to make room when at capacity.
    ///
    /// Tracks are evicted in priority order: `Finished` first, then `FadingOut`,
    /// `Preloading`, `Paused`, `FadingIn`, and `Playing` last. If all tracks are in the
    /// same state (e.g. all Playing), eviction order is non-deterministic
    /// because `HashMap` iteration order is undefined.
    pub(super) fn evict_tracks_if_needed(&mut self) {
        while self.tracks.len() >= Self::MAX_TRACKS && !self.retirement_blocked() {
            let eviction_candidate = self
                .tracks
                .iter_keys()
                .min_by_key(|(_, idx)| {
                    self.tracks
                        .get_by_index(**idx)
                        .map_or(0, |t| super::render::eviction_priority(t.state()))
                })
                .map(|(key, idx)| {
                    let state = self.tracks.get_by_index(*idx).map(PlayerTrack::state);
                    (Arc::clone(key), state)
                });

            if let Some((key, state)) = eviction_candidate {
                if state == Some(TrackState::Playing) {
                    warn!(
                        src = &*key,
                        "evicting a Playing track to make room for a new track"
                    );
                }
                if let Some(track) = self.tracks.remove(&key) {
                    self.discard_track(track);
                    self.notif_tx
                        .try_push(PlayerNotification::Unloaded { src: key })
                        .ok();
                }
            } else {
                break;
            }
        }
    }

    fn flush_retirement(&mut self) {
        let Some(track) = self.pending_retirement.take() else {
            return;
        };
        if let Err(track) = self.trash_tx.try_push(track) {
            self.pending_retirement = Some(track);
        }
    }

    pub fn render_audio(
        &mut self,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        self.render_with_mode(TrackRenderMode::Standalone, buffers, frames, is_playing)
    }

    fn render_with_mode(
        &mut self,
        mode: TrackRenderMode<'_>,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        let (playback_started, leading_outcome, multiple_tracks) = self.render.render_audio(
            mode,
            RenderTargets {
                tracks: &mut self.tracks,
                notification_tx: &mut self.notif_tx,
            },
            buffers,
            frames,
            is_playing,
        );
        self.playback
            .multiple_tracks
            .store(multiple_tracks, Ordering::SeqCst);
        if let TrackRenderMode::Session(context) = mode
            && self.session_seek.is_some_and(|pending| {
                context.transport_revision() == Some(pending.revision)
                    && context.transport_boundary()
                        == Some(TransportBoundary::Relocate(pending.target))
            })
        {
            self.session_seek = None;
        }
        (playback_started, leading_outcome)
    }

    pub(super) fn retirement_blocked(&self) -> bool {
        self.pending_retirement.is_some()
    }

    fn set_tracks_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.tracks
            .iter_mut()
            .for_each(|(_, track)| track.update_host_sample_rate(sample_rate));
    }

    /// Unload a track from the arena.
    pub(super) fn unload_track(&mut self, src: &Arc<str>) {
        if let Some(track) = self.tracks.remove(src) {
            self.discard_track(track);
            self.notif_tx
                .try_push(PlayerNotification::Unloaded {
                    src: Arc::clone(src),
                })
                .ok();
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
    fn update_position_duration(&self, leading_outcome: Option<(f64, f64)>) {
        // Decoded-ahead frontier comes from the leading track's lock-free
        // snapshot (always `>=` position) — the authoritative buffered/playable
        // window the FFI polls for loaded ranges.
        let leading = self
            .tracks
            .iter()
            .find_map(|(_, track)| track.state().is_leading().then_some(track));
        if let Some(track) = leading {
            self.playback
                .frontier
                .store(track.decoded_frontier(), Ordering::Relaxed);
        }

        if let Some((position, duration)) = leading_outcome {
            self.playback.position.store(position, Ordering::Relaxed);
            self.playback.duration.store(duration, Ordering::Relaxed);
            return;
        }

        if let Some(track) = leading {
            self.playback
                .position
                .store(track.position(), Ordering::Relaxed);
            self.playback
                .duration
                .store(track.duration(), Ordering::Relaxed);
        }
    }

    pub(crate) fn with_context_requirement(
        inputs: NodeInputs,
        shape: StreamShape,
        pool: &PcmPool,
        context_requirement: ContextRequirement,
    ) -> Self {
        Self {
            context_requirement,
            cmd_rx: inputs.cmd_rx,
            notif_tx: inputs.notif_tx,
            trash_tx: inputs.trash_tx,
            pending_retirement: None,
            playback: inputs.playback,
            sample_rate: shape.sample_rate,
            render: RenderPass::new(pool, shape.max_block_frames.get().as_()),
            crossfade: CrossfadeSettings::default(),
            prefetch_duration: 0.0,
            tracks: ArenaRegistry::with_capacity(Self::MAX_TRACKS),
            tracks_transitions: VecDeque::with_capacity(Self::MAX_TRACKS),
            session_seek: None,
        }
    }

    delegate::delegate! {
        to self.tracks {
            /// Look up a track by its source identifier.
            #[must_use]
            #[call(get)]
            pub fn track(&self, src: &Arc<str>) -> Option<&PlayerTrack>;
            /// Number of tracks currently held in the processor arena.
            #[must_use]
            #[call(len)]
            pub fn track_count(&self) -> usize;
            /// Look up a track by its source identifier (mutable).
            #[call(get_mut)]
            pub fn track_mut(&mut self, src: &Arc<str>) -> Option<&mut PlayerTrack>;
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
        _events: &mut ProcEvents,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        self.playback.process_count.fetch_add(1, Ordering::Relaxed);

        self.flush_retirement();
        if !self.retirement_blocked() {
            self.drain_commands();
            self.cleanup_finished_tracks();
        }

        let is_playing = self.playback.playing.load(Ordering::SeqCst);

        let render_mode = match self.context_requirement {
            ContextRequirement::Standalone => TrackRenderMode::Standalone,
            ContextRequirement::Session => match read_render_context(&extra.store, info) {
                Ok(context) => TrackRenderMode::Session(context),
                Err(reason) => {
                    let _ = extra.logger.try_error(reason.message());
                    return ProcessStatus::ClearAllOutputs;
                }
            },
        };

        let (playback_started, leading_outcome_pos_dur) =
            self.render_with_mode(render_mode, &mut buffers, info.frames, is_playing);
        self.poll_session_seek();
        self.update_position_duration(leading_outcome_pos_dur);

        if playback_started {
            ProcessStatus::OutputsModified
        } else {
            ProcessStatus::ClearAllOutputs
        }
    }
}

#[cfg(test)]
mod tests {
    use ringbuf::traits::{Consumer, Producer};

    use super::*;
    use crate::{
        api::{SessionBeat, Tempo, TransportRevision},
        bridge::{PlayerCmd, SharedEq, SlotControl, TrackStart, slot_channels},
        player::track::PlayerResource,
        test_support::empty_resource,
    };

    fn processor_and_control() -> (PlayerNodeProcessor, SlotControl) {
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let shape = StreamShape {
            sample_rate: NonZeroU32::new(44_100).expect("static sample rate"),
            max_block_frames: NonZeroU32::new(512).expect("static block size"),
        };
        (
            PlayerNodeProcessor::new(inputs, shape, &PcmPool::default()),
            control,
        )
    }

    fn load(src: &Arc<str>) -> PlayerCmd {
        let resource =
            PlayerResource::new(empty_resource(src), Arc::clone(src), &PcmPool::default());
        PlayerCmd::LoadTrack {
            binding: None,
            resource,
            item_id: None,
            start: TrackStart::Immediate,
        }
    }

    fn pending_seek(revision: u64, target: f64) -> PendingSessionSeek {
        PendingSessionSeek {
            target: SessionBeat::new(target).expect("finite pending target"),
            attempt: SessionSeekAttempt::new_for_test(revision),
            revision: TransportRevision::new_for_test(revision),
        }
    }

    fn assert_track_mutation_aborts_pending_seek(command: PlayerCmd) {
        let (mut processor, mut control) = processor_and_control();
        let src: Arc<str> = Arc::from("pending-seek.wav");
        control
            .cmd_tx
            .try_push(load(&src))
            .expect("load command fits");
        processor.drain_commands();
        let pending = pending_seek(7, 1.0);
        processor.session_seek = Some(pending);

        control
            .cmd_tx
            .try_push(command)
            .expect("mutation command fits");
        processor.drain_commands();

        assert_eq!(processor.session_seek, None);
        assert!(processor.playback.session_seek_failed(pending.attempt));
    }

    #[kithara::test]
    fn track_mutations_abort_a_pending_session_seek() {
        let src: Arc<str> = Arc::from("pending-seek.wav");
        assert_track_mutation_aborts_pending_seek(PlayerCmd::UnloadTrack {
            src: Arc::clone(&src),
        });
        assert_track_mutation_aborts_pending_seek(PlayerCmd::Transition(TrackTransition::FadeIn(
            Arc::clone(&src),
        )));
        assert_track_mutation_aborts_pending_seek(PlayerCmd::Clear);
        assert_track_mutation_aborts_pending_seek(load(&src));
    }

    #[kithara::test]
    fn session_seek_cancel_clears_the_matching_preparation() {
        let (mut processor, _control) = processor_and_control();
        let pending = pending_seek(7, 1.0);
        processor.session_seek = Some(pending);
        processor.playback.prepare_session_seek(pending.attempt);

        processor.playback.cancel_session_seek(pending.attempt);
        processor.poll_session_seek();

        assert_eq!(processor.session_seek, None);
        assert!(!processor.playback.session_seek_prepared(pending.attempt));
        assert!(!processor.playback.session_seek_cancelled(pending.attempt));
    }

    #[kithara::test]
    fn second_session_seek_prepare_releases_the_stale_owner() {
        let (mut processor, mut control) = processor_and_control();
        let first = pending_seek(7, 1.0);
        let second = pending_seek(8, 2.0);
        processor.session_seek = Some(first);

        control
            .cmd_tx
            .try_push(PlayerCmd::PrepareSessionSeek {
                attempt: second.attempt,
                target: second.target,
                tempo: Tempo::new(120.0).expect("valid tempo"),
                revision: second.revision,
            })
            .expect("second prepare command fits");
        processor.drain_commands();

        assert_eq!(processor.session_seek, None);
        assert!(!processor.playback.session_seek_prepared(first.attempt));
        assert!(processor.playback.session_seek_failed(second.attempt));
    }

    #[kithara::test]
    fn saturated_retirement_lane_preserves_the_pending_track_and_command_order() {
        let (mut processor, mut control) = processor_and_control();
        let src: Arc<str> = Arc::from("replace.wav");

        for _ in 0..66 {
            control
                .cmd_tx
                .try_push(load(&src))
                .expect("load command capacity is drained each iteration");
            processor.drain_commands();
        }
        assert!(processor.retirement_blocked());
        assert_eq!(processor.track_count(), 1);

        control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused(false))
            .expect("one command remains available");
        processor.drain_commands();
        assert!(!processor.playback.playing.load(Ordering::SeqCst));

        let mut retired = 0;
        while control.trash_rx.try_pop().is_some() {
            retired += 1;
        }
        assert_eq!(retired, 64);

        processor.flush_retirement();
        assert!(!processor.retirement_blocked());
        processor.drain_commands();
        assert!(processor.playback.playing.load(Ordering::SeqCst));
        assert!(control.trash_rx.try_pop().is_some());
        assert!(control.trash_rx.try_pop().is_none());
    }
}
