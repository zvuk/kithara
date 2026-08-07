use std::{collections::VecDeque, num::NonZeroU32, sync::atomic::Ordering};

use bon::Builder;
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
use smallvec::SmallVec;

use super::track::PlayerTrack;
use crate::{
    bridge::{
        NodeInputs, PlaybackShared, PlayerCmd, PlayerNotification, TrackState, TrackTransition,
    },
    rt::{RenderPass, RenderTargets, TrackSlot, TrackSlots},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum CrossfadeCurve {
    #[default]
    EqualPower,
}

fn map_curve(curve: CrossfadeCurve) -> FadeCurve {
    match curve {
        CrossfadeCurve::EqualPower => FadeCurve::SquareRoot,
    }
}

#[derive(Clone, Debug, Builder)]
pub(crate) struct CrossfadeSettings {
    #[builder(default)]
    pub(crate) curve: CrossfadeCurve,
    #[builder(default = 1.0)]
    pub(crate) duration: f32,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl CrossfadeSettings {
    pub(crate) fn fade_curve(&self) -> FadeCurve {
        map_curve(self.curve)
    }
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
    pub(super) tracks: TrackSlots<{ Self::MAX_TRACKS }>,
    pub(super) crossfade: CrossfadeSettings,
    pub(super) cmd_rx: HeapCons<PlayerCmd>,
    pub(super) notif_tx: HeapProd<PlayerNotification>,
    pub(super) sample_rate: NonZeroU32,
    pub(super) render: RenderPass,
    pub(super) tracks_transitions: VecDeque<TrackTransition>,
    /// Media seconds consumed per output second, applied to every track the
    /// processor owns and seeded into every track it loads.
    pub(super) playback_rate: f32,
    pub(super) prefetch_duration: f32,
    trash_tx: HeapProd<PlayerTrack>,
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy)]
pub struct StreamShape {
    pub max_block_frames: NonZeroU32,
    pub sample_rate: NonZeroU32,
}

impl PlayerNodeProcessor {
    /// Minimum position (seconds) before seeking is allowed on fade-in.
    pub(super) const FADE_IN_SEEK_THRESHOLD: f64 = 0.5;

    /// Maximum number of concurrent tracks per player node.
    pub const MAX_TRACKS: usize = 4;

    /// Create a new processor with the given command receiver and shared state.
    #[must_use]
    pub fn new(inputs: NodeInputs, shape: StreamShape, pool: &PcmPool) -> Self {
        Self {
            cmd_rx: inputs.cmd_rx,
            notif_tx: inputs.notif_tx,
            trash_tx: inputs.trash_tx,
            playback: inputs.playback,
            sample_rate: shape.sample_rate,
            render: RenderPass::new(pool, shape),
            crossfade: CrossfadeSettings::default(),
            prefetch_duration: 0.0,
            playback_rate: 1.0,
            tracks: TrackSlots::default(),
            tracks_transitions: VecDeque::with_capacity(Self::MAX_TRACKS),
        }
    }

    /// Clean up finished tracks, dropping `playing` once none is audible.
    pub fn cleanup_finished_tracks(&mut self) {
        let finished: SmallVec<[(TrackSlot, bool); Self::MAX_TRACKS]> = self
            .tracks
            .iter()
            .filter(|(_, track)| track.state() == TrackState::Finished)
            .map(|(slot, track)| (slot, track.ended_at_eof()))
            .collect();

        // Superpowered-style end-of-queue resume: if removing the finished tracks would empty the
        // slot set (the queue has played out) and one of them reached *natural* EOF, keep that
        // single track resident (warm) so a later in-range seek can revive it (`apply_seek`). It is
        // reclaimed by `evict_tracks_if_needed` (Finished evicts first) when the next track loads.
        // Tracks that finished via `stop()` or a faded-out crossfade (not `ended_at_eof`) are
        // discarded as usual.
        let retain: Option<TrackSlot> = if finished.len() == self.tracks.len() {
            finished
                .iter()
                .find_map(|(slot, ended_at_eof)| ended_at_eof.then_some(*slot))
        } else {
            None
        };

        for (slot, _) in finished.iter().filter(|(slot, _)| Some(*slot) != retain) {
            if let Some(track) = self.tracks.remove_at(*slot) {
                let src = Arc::clone(track.src());
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded { src })
                    .ok();
            }
        }

        // The retained track is `Finished`, so `render_audio` skips it and `is_playing()` stays
        // false until a seek revives it.
        if self.tracks.len() == 0 || retain.is_some() {
            self.playback.playing.store(false, Ordering::SeqCst);
        }
    }

    pub(super) fn discard_track(&mut self, track: PlayerTrack) {
        if self.trash_tx.try_push(track).is_err() {
            self.playback.metrics().record_trash_overflow();
        }
    }

    pub(super) fn evict_tracks_if_needed(&mut self) {
        while self.tracks.is_full() {
            let Some((slot, state)) = self
                .tracks
                .iter()
                .min_by_key(|(_, track)| super::render::eviction_priority(track.state()))
                .map(|(slot, track)| (slot, track.state()))
            else {
                break;
            };

            if state == TrackState::Playing {
                self.playback.metrics().record_evicted_playing();
            }
            if let Some(track) = self.tracks.remove_at(slot) {
                let src = Arc::clone(track.src());
                self.discard_track(track);
                self.notif_tx
                    .try_push(PlayerNotification::Unloaded { src })
                    .ok();
            }
        }
    }

    pub fn render_audio(
        &mut self,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        self.render.render_audio(
            RenderTargets {
                tracks: &mut self.tracks,
                notification_tx: &mut self.notif_tx,
                metrics: self.playback.metrics(),
            },
            buffers,
            frames,
            is_playing,
        )
    }

    fn set_tracks_host_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.tracks
            .iter()
            .for_each(|(_, track)| track.resource().set_host_sample_rate(sample_rate));
    }

    pub(super) fn unload_track(&mut self, src: &str) {
        if let Some(track) = self.tracks.remove(src) {
            self.retire(track);
        }
    }

    pub(super) fn unload_slot(&mut self, slot: TrackSlot) {
        if let Some(track) = self.tracks.remove_at(slot) {
            self.retire(track);
        }
    }

    fn retire(&mut self, track: PlayerTrack) {
        let src = Arc::clone(track.src());
        self.discard_track(track);
        self.notif_tx
            .try_push(PlayerNotification::Unloaded { src })
            .ok();
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
    fn update_position_duration(&self, leading_outcome: Option<(f64, f64)>) {
        // Both windows come from the leading track's lock-free snapshots: the
        // decoded frontier (always `>=` position) and the cached span the
        // download side published. The queue view unions them into the
        // buffered window the FFI polls for loaded ranges.
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
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        self.playback.process_count.fetch_add(1, Ordering::Relaxed);

        self.drain_commands();

        self.cleanup_finished_tracks();

        let is_playing = self.playback.playing.load(Ordering::SeqCst);

        let (playback_started, leading_outcome_pos_dur) =
            self.render_audio(&mut buffers, info.frames, is_playing);

        self.update_position_duration(leading_outcome_pos_dur);

        if playback_started {
            ProcessStatus::OutputsModified
        } else {
            ProcessStatus::ClearAllOutputs
        }
    }
}
