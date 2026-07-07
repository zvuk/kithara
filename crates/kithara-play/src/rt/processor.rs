use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
};

use firewheel::{
    StreamInfo,
    dsp::fade::FadeCurve,
    event::ProcEvents,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;
use ringbuf::{HeapCons, HeapProd, traits::Producer};
use thunderdome::Index;
use tracing::warn;

use super::track::PlayerTrack;
use crate::{
    bridge::{
        NodeInputs, PlaybackShared, PlayerCmd, PlayerNotification, TrackState, TrackTransition,
    },
    rt::ArenaRegistry,
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
pub struct PlayerNodeProcessor {
    pub(super) playback: Arc<PlaybackShared>,
    pub(super) tracks: ArenaRegistry<Arc<str>, PlayerTrack>,
    pub(super) crossfade: CrossfadeSettings,
    pub(super) cmd_rx: HeapCons<PlayerCmd>,
    pub(super) notif_tx: HeapProd<PlayerNotification>,
    trash_tx: HeapProd<PlayerTrack>,
    pub(super) sample_rate: NonZeroU32,
    pub(super) tracks_transitions: VecDeque<TrackTransition>,
    pub(super) scratch_bufs: [PcmBuf; Self::SCRATCH_BUF_COUNT],
    pub(super) prefetch_duration: f32,
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy)]
pub struct StreamShape {
    pub sample_rate: NonZeroU32,
    pub max_block_frames: NonZeroU32,
}

impl PlayerNodeProcessor {
    /// Minimum position (seconds) before seeking is allowed on fade-in.
    pub(super) const FADE_IN_SEEK_THRESHOLD: f64 = 0.5;

    /// Maximum number of concurrent tracks per player node.
    pub(super) const MAX_TRACKS: usize = 4;

    /// Minimum stereo channel count for output processing.
    pub(super) const MIN_STEREO: usize = 2;

    /// Number of scratch buffers for stereo processing.
    const SCRATCH_BUF_COUNT: usize = 4;

    /// Create a new processor with the given command receiver and shared state.
    #[must_use]
    pub fn new(inputs: NodeInputs, shape: StreamShape, pool: &PcmPool) -> Self {
        let max_frames: usize = shape.max_block_frames.get().as_();
        let scratch_bufs = std::array::from_fn(|_| {
            let mut buf = pool.get();
            let cap = buf.capacity();
            if cap < max_frames {
                buf.reserve(max_frames - cap);
            }
            buf
        });

        Self {
            cmd_rx: inputs.cmd_rx,
            notif_tx: inputs.notif_tx,
            trash_tx: inputs.trash_tx,
            playback: inputs.playback,
            sample_rate: shape.sample_rate,
            scratch_bufs,
            crossfade: CrossfadeSettings::default(),
            prefetch_duration: 0.0,
            tracks: ArenaRegistry::with_capacity(Self::MAX_TRACKS),
            tracks_transitions: VecDeque::with_capacity(Self::MAX_TRACKS),
        }
    }

    /// Update fade duration for all tracks.
    pub(super) fn discard_track(&mut self, track: PlayerTrack) {
        let _ = self.trash_tx.try_push(track);
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

        for (key, idx) in self.tracks.iter_keys() {
            if let Some(track) = self.tracks.get_by_index(*idx)
                && track.state() == TrackState::Finished
                && count < Self::MAX_TRACKS
            {
                finished[count] = Some((Arc::clone(key), *idx));
                count += 1;
            }
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

    /// Evict tracks to make room when at capacity.
    ///
    /// Tracks are evicted in priority order: `Finished` first, then `FadingOut`,
    /// `Preloading`, `Paused`, `FadingIn`, and `Playing` last. If all tracks are in the
    /// same state (e.g. all Playing), eviction order is non-deterministic
    /// because `HashMap` iteration order is undefined.
    pub(super) fn evict_tracks_if_needed(&mut self) {
        while self.tracks.len() >= Self::MAX_TRACKS {
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

    /// Reference to the playback atomics used by the processor.
    #[must_use]
    pub fn playback(&self) -> &Arc<PlaybackShared> {
        &self.playback
    }

    /// Look up a track by its source identifier.
    #[must_use]
    pub fn track(&self, src: &Arc<str>) -> Option<&PlayerTrack> {
        self.tracks.get(src)
    }

    /// Number of tracks currently held in the processor arena.
    #[must_use]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Look up a track by its source identifier (mutable).
    pub fn track_mut(&mut self, src: &Arc<str>) -> Option<&mut PlayerTrack> {
        self.tracks.get_mut(src)
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
        for (_, track) in self.tracks.iter() {
            if track.state().is_leading() {
                self.playback
                    .frontier
                    .store(track.decoded_frontier(), Ordering::Relaxed);
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
}

impl AudioNodeProcessor for PlayerNodeProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        let new_sr = stream_info.sample_rate;
        self.sample_rate = new_sr;
        self.playback
            .sample_rate
            .store(new_sr.get(), Ordering::Relaxed);

        let max_frames: usize = stream_info.max_block_frames.get().as_();
        for buf in &mut self.scratch_bufs {
            let cap = buf.capacity();
            if cap < max_frames {
                buf.reserve(max_frames - cap);
            }
        }

        self.tracks
            .iter()
            .for_each(|(_, track)| track.resource().set_host_sample_rate(new_sr));
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
