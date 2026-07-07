use std::{
    collections::VecDeque,
    fmt,
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
};

use firewheel::{
    StreamInfo,
    event::ProcEvents,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_test_utils::kithara;
use num_traits::cast::AsPrimitive;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Producer},
};
use smallvec::SmallVec;
use thunderdome::Index;
use tracing::warn;

use super::{
    arena_registry::ArenaRegistry,
    crossfade::CrossfadeSettings,
    player_notification::PlayerNotification,
    player_resource::PlayerResource,
    player_track::{PlayerTrack, TrackParams, TrackReadOutcome, TrackState, TrackTransition},
};
use crate::bridge::{NodeInputs, PlaybackShared};

/// Commands sent from the main thread to the processor.
pub enum PlayerCmd {
    /// Load a track into the processor arena.
    LoadTrack {
        resource: Box<PlayerResource>,
        item_id: Option<Arc<str>>,
        src: Arc<str>,
    },
    /// Unload a track by its source identifier.
    UnloadTrack { src: Arc<str> },
    /// Unload every track from the arena and reset the position/duration
    /// snapshot to zero. Sent when the queue is explicitly cleared.
    Clear,
    /// Add a track transition (fade in / fade out).
    Transition(TrackTransition),
    /// Seek active tracks to the given position in seconds.
    Seek { seconds: f64, seek_epoch: u64 },
    /// Set the paused state.
    SetPaused(bool),
    /// Update the fade duration.
    SetFadeDuration(f32),
    /// Update the prefetch lead time.
    SetPrefetchDuration(f32),
    /// Update the playback rate for all active tracks.
    SetPlaybackRate(f32),
}

impl fmt::Debug for PlayerCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadTrack { item_id, src, .. } => f
                .debug_struct("LoadTrack")
                .field("item_id", item_id)
                .field("src", src)
                .finish_non_exhaustive(),
            Self::UnloadTrack { src } => f.debug_struct("UnloadTrack").field("src", src).finish(),
            Self::Clear => f.write_str("Clear"),
            Self::Transition(t) => f.debug_tuple("Transition").field(t).finish(),
            Self::Seek {
                seconds,
                seek_epoch,
            } => f
                .debug_struct("Seek")
                .field("seconds", seconds)
                .field("seek_epoch", seek_epoch)
                .finish(),
            Self::SetPaused(p) => f.debug_tuple("SetPaused").field(p).finish(),
            Self::SetFadeDuration(d) => f.debug_tuple("SetFadeDuration").field(d).finish(),
            Self::SetPrefetchDuration(d) => f.debug_tuple("SetPrefetchDuration").field(d).finish(),
            Self::SetPlaybackRate(r) => f.debug_tuple("SetPlaybackRate").field(r).finish(),
        }
    }
}

/// The realtime audio processor for the player node.
///
/// `(arena_slot, handle, was_leading)` tuple captured per active track for
/// one `process()` call. Lives only inside `render_audio` — exposed as a
/// type alias so the `SmallVec` literal stays under the workspace
/// `type_complexity` threshold.
type ActiveTrackEntry = (usize, Index, bool);

/// Manages tracks in a thunderdome arena, handles transitions,
/// and renders mixed stereo audio into the Firewheel output buffers.
pub struct PlayerNodeProcessor {
    playback: Arc<PlaybackShared>,
    tracks: ArenaRegistry<Arc<str>, PlayerTrack>,
    crossfade: CrossfadeSettings,
    cmd_rx: HeapCons<PlayerCmd>,
    notif_tx: HeapProd<PlayerNotification>,
    trash_tx: HeapProd<PlayerTrack>,
    sample_rate: NonZeroU32,
    tracks_transitions: VecDeque<TrackTransition>,
    scratch_bufs: [PcmBuf; Self::SCRATCH_BUF_COUNT],
    prefetch_duration: f32,
}

/// Stream dimensions needed to pre-size RT scratch buffers.
#[derive(Clone, Copy)]
pub struct StreamShape {
    pub sample_rate: NonZeroU32,
    pub max_block_frames: NonZeroU32,
}

impl PlayerNodeProcessor {
    /// Minimum position (seconds) before seeking is allowed on fade-in.
    const FADE_IN_SEEK_THRESHOLD: f64 = 0.5;

    /// Maximum number of concurrent tracks per player node.
    const MAX_TRACKS: usize = 4;

    /// Minimum stereo channel count for output processing.
    const MIN_STEREO: usize = 2;

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
    fn apply_fade_duration(&mut self, duration: f32) {
        self.crossfade.duration = duration;
        for (_, track) in self.tracks.iter_mut() {
            track.update_fade_duration(duration, self.sample_rate);
        }
    }

    /// Update prefetch lead time for all tracks.
    fn apply_prefetch_duration(&mut self, duration: f32) {
        self.prefetch_duration = duration.max(0.0);
        for (_, track) in self.tracks.iter_mut() {
            track.set_prefetch_duration(self.prefetch_duration);
        }
    }

    /// Apply seek to active tracks.
    fn apply_seek(&mut self, seconds: f64, seek_epoch: u64) {
        if seek_epoch != self.playback.seek_epoch.load(Ordering::SeqCst) {
            return;
        }

        let mut revived = false;
        for (_, track) in self.tracks.iter_mut() {
            match track.state() {
                TrackState::FadingIn | TrackState::Playing => {
                    track.seek(seconds);
                    track.play();
                }
                TrackState::FadingOut => {
                    track.stop();
                }
                // Superpowered-style resume: a played-out track kept warm at
                // end-of-queue (see `cleanup_finished_tracks`) resumes from an
                // in-range seek. A seek at/past duration leaves it ended (the
                // `< duration` guard skips revival), preserving PastEof.
                TrackState::Finished if track.ended_at_eof() && seconds < track.duration() => {
                    track.seek(seconds);
                    track.play();
                    revived = true;
                }
                _ => {}
            }
        }
        if revived {
            self.playback.playing.store(true, Ordering::SeqCst);
        }
    }

    fn discard_track(&mut self, track: PlayerTrack) {
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

    /// Unload every track from the arena and reset the published
    /// position/duration snapshot to zero. Backs [`PlayerCmd::Clear`],
    /// sent when the queue is explicitly cleared, so a subsequent read of
    /// `playback` reflects an empty player instead of a stale snapshot.
    fn clear_all_tracks(&mut self) {
        let keys: SmallVec<[Arc<str>; Self::MAX_TRACKS]> = self
            .tracks
            .iter_keys()
            .map(|(key, _)| Arc::clone(key))
            .collect();
        for key in keys {
            self.unload_track(&key);
        }
        self.tracks_transitions.clear();
        self.playback.playing.store(false, Ordering::SeqCst);
        self.playback.position.store(0.0, Ordering::Relaxed);
        self.playback.frontier.store(0.0, Ordering::Relaxed);
        self.playback.duration.store(0.0, Ordering::Relaxed);
    }

    /// Drain all pending commands from the channel.
    pub fn drain_commands(&mut self) {
        while let Some(cmd) = self.cmd_rx.try_pop() {
            match cmd {
                PlayerCmd::LoadTrack {
                    resource,
                    item_id,
                    src,
                } => {
                    self.load_track(resource, item_id, src);
                }
                PlayerCmd::UnloadTrack { src } => {
                    self.unload_track(&src);
                }
                PlayerCmd::Clear => {
                    self.clear_all_tracks();
                }
                PlayerCmd::Transition(transition) => {
                    self.handle_transition(transition);
                }
                PlayerCmd::Seek {
                    seconds,
                    seek_epoch,
                } => {
                    self.apply_seek(seconds, seek_epoch);
                }
                PlayerCmd::SetPaused(paused) => {
                    let playing = !paused;
                    self.playback.playing.store(playing, Ordering::SeqCst);
                }
                PlayerCmd::SetFadeDuration(duration) => {
                    self.apply_fade_duration(duration);
                }
                PlayerCmd::SetPrefetchDuration(duration) => {
                    self.apply_prefetch_duration(duration);
                }
                PlayerCmd::SetPlaybackRate(rate) => {
                    self.tracks
                        .iter()
                        .for_each(|(_, track)| track.resource().set_playback_rate(rate));
                }
            }
        }
    }

    /// Evict tracks to make room when at capacity.
    ///
    /// Tracks are evicted in priority order: `Finished` first, then `FadingOut`,
    /// `Preloading`, `Paused`, `FadingIn`, and `Playing` last. If all tracks are in the
    /// same state (e.g. all Playing), eviction order is non-deterministic
    /// because `HashMap` iteration order is undefined.
    fn evict_tracks_if_needed(&mut self) {
        while self.tracks.len() >= Self::MAX_TRACKS {
            let eviction_candidate = self
                .tracks
                .iter_keys()
                .min_by_key(|(_, idx)| {
                    self.tracks
                        .get_by_index(**idx)
                        .map_or(0, |t| eviction_priority(t.state()))
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

    /// Handle a track transition command.
    fn handle_transition(&mut self, transition: TrackTransition) {
        let (mut old_track, mut new_track) = (None, None);

        if let TrackTransition::FadeIn(ref nt) = transition {
            new_track = Some(Arc::clone(nt));

            self.tracks_transitions.clear();

            let maybe_old = self.tracks.iter_keys().find_map(|(key, idx)| {
                self.tracks
                    .get_by_index(*idx)
                    .filter(|t| t.state().is_leading())
                    .map(|_| Arc::clone(key))
            });

            if let Some(ref ot) = maybe_old
                && ot != nt
            {
                old_track = Some(Arc::clone(ot));
                self.tracks_transitions
                    .push_back(TrackTransition::FadeOut(Arc::clone(ot)));
            }
        }

        self.tracks_transitions.push_back(transition);

        let playback = Arc::clone(&self.playback);
        self.tracks_transitions.retain(|transition| {
            let track_src = match transition {
                TrackTransition::FadeIn(src) | TrackTransition::FadeOut(src) => Arc::clone(src),
            };
            if let Some(track) = self.tracks.get_mut(&track_src) {
                match transition {
                    TrackTransition::FadeIn(_) => {
                        if track.position() > Self::FADE_IN_SEEK_THRESHOLD {
                            track.seek(0.0);
                        }
                        track.fade_in();
                        playback.position.store(track.position(), Ordering::Relaxed);
                        playback.duration.store(track.duration(), Ordering::Relaxed);
                    }
                    TrackTransition::FadeOut(_) => {
                        track.fade_out();
                    }
                }
                return false;
            }
            true
        });

        if old_track.is_some()
            && let Some(new_src) = new_track
        {
            self.notif_tx
                .try_push(PlayerNotification::Changed { src: new_src })
                .ok();
        }
    }

    fn initial_handover_offset(read_outcome: &TrackReadOutcome) -> Option<usize> {
        match read_outcome {
            TrackReadOutcome::Partial { frames, .. } => Some(*frames),
            TrackReadOutcome::Eof | TrackReadOutcome::Failed => Some(0),
            TrackReadOutcome::Full { .. } => None,
        }
    }

    /// Load a new track into the arena.
    fn load_track(
        &mut self,
        resource: Box<PlayerResource>,
        item_id: Option<Arc<str>>,
        src: Arc<str>,
    ) {
        if let Some(track) = self.tracks.remove(&src) {
            self.discard_track(track);
            self.notif_tx
                .try_push(PlayerNotification::Unloaded {
                    src: Arc::clone(&src),
                })
                .ok();
        }

        self.evict_tracks_if_needed();

        let resource = *resource;
        resource.set_host_sample_rate(self.sample_rate);

        let loaded_src = Arc::clone(&src);
        let params = TrackParams::new(Arc::clone(&src), self.sample_rate)
            .with_item_id(item_id)
            .with_fade_duration(self.crossfade.duration)
            .with_prefetch_duration(self.prefetch_duration)
            .with_fade_curve(self.crossfade.fade_curve());
        let track = PlayerTrack::new(resource, params);
        self.tracks.insert(src, track);

        self.notif_tx
            .try_push(PlayerNotification::Loaded { src: loaded_src })
            .ok();
    }

    fn next_handover_offset(read_outcome: &TrackReadOutcome, offset: usize) -> Option<usize> {
        match read_outcome {
            TrackReadOutcome::Full { .. } => None,
            TrackReadOutcome::Partial { frames, .. } => Some(offset + *frames),
            TrackReadOutcome::Eof | TrackReadOutcome::Failed => Some(offset),
        }
    }

    /// Captured position/duration snapshot from the leading track's read
    /// outcome, in the same units as `playback.position` /
    /// `playback.duration`.
    fn outcome_position_duration(outcome: &TrackReadOutcome) -> Option<(f64, f64)> {
        match *outcome {
            TrackReadOutcome::Full {
                position, duration, ..
            } => Some((position, duration)),
            TrackReadOutcome::Partial { duration, .. } => Some((duration, duration)),
            TrackReadOutcome::Eof | TrackReadOutcome::Failed => None,
        }
    }

    /// Render audio for all active tracks into the output buffers.
    ///
    /// Returns `(playback_started, leading_outcome_pos_dur)`:
    /// * `playback_started` — whether at least one track produced audio
    ///   this block (drives the firewheel `OutputsModified` /
    ///   `ClearAllOutputs` decision in [`Self::process`]).
    /// * `leading_outcome_pos_dur` — the leading track's
    ///   position/duration snapshot lifted out of [`TrackReadOutcome`]
    ///   so [`Self::update_position_duration`] can publish it to
    ///   `playback` without re-locking the resource.
    pub fn render_audio(
        &mut self,
        buffers: &mut ProcBuffers,
        frames: usize,
        is_playing: bool,
    ) -> (bool, Option<(f64, f64)>) {
        let mut playback_started = false;
        let mut leading_outcome_pos_dur: Option<(f64, f64)> = None;

        if buffers.outputs.len() < Self::MIN_STEREO {
            return (false, None);
        }

        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }

        for buf in &mut self.scratch_bufs {
            buf.resize(frames, 0.0);
        }

        let (left, right) = self.scratch_bufs.split_at_mut(Self::MIN_STEREO);
        let (read_buf0, read_buf1) = left.split_at_mut(1);
        let (mix_buf0, mix_buf1) = right.split_at_mut(1);
        let mut read_bufs = [&mut read_buf0[0][..frames], &mut read_buf1[0][..frames]];
        let mut mix_bufs = [&mut mix_buf0[0][..frames], &mut mix_buf1[0][..frames]];
        let notification_tx = &mut self.notif_tx;
        let tracks = &mut self.tracks;
        let arena_tracks: SmallVec<[(Index, TrackState); Self::MAX_TRACKS]> = if is_playing {
            tracks
                .iter()
                .map(|(idx, track)| (idx, track.state()))
                .collect()
        } else {
            SmallVec::new()
        };
        let active_tracks: SmallVec<[ActiveTrackEntry; Self::MAX_TRACKS]> = arena_tracks
            .iter()
            .enumerate()
            .filter(|(_, (_, state))| state.is_playing())
            .map(|(arena_idx, (idx, state))| (arena_idx, *idx, state.is_leading()))
            .collect();
        let mut active_arena_slots = [false; Self::MAX_TRACKS];
        for (arena_idx, _, _) in &active_tracks {
            active_arena_slots[*arena_idx] = true;
        }
        let mut skip_tracks = [false; Self::MAX_TRACKS];

        for (track_idx, (_arena_slot, track_handle, was_leading)) in
            active_tracks.iter().enumerate()
        {
            if skip_tracks[track_idx] {
                continue;
            }

            for ch_buffer in &mut mix_bufs {
                ch_buffer.fill(0.0);
            }

            let mut read_outcome = {
                let Some(outcome) = tracks.get_by_index_mut(*track_handle).map(|track| {
                    track.read(&mut read_bufs, &mut mix_bufs, 0..frames, notification_tx)
                }) else {
                    continue;
                };
                playback_started = true;
                outcome
            };

            if *was_leading {
                if let Some(snapshot) = Self::outcome_position_duration(&read_outcome) {
                    leading_outcome_pos_dur = Some(snapshot);
                }

                let mut handover_offset = Self::initial_handover_offset(&read_outcome);

                for (next_idx, (_, next_handle, next_is_leading)) in
                    active_tracks.iter().enumerate()
                {
                    let Some(offset) = handover_offset else {
                        break;
                    };
                    if next_idx == track_idx || skip_tracks[next_idx] || !*next_is_leading {
                        continue;
                    }
                    if offset >= frames {
                        break;
                    }

                    let Some(outcome) = tracks.get_by_index_mut(*next_handle).map(|track| {
                        track.read(
                            &mut read_bufs,
                            &mut mix_bufs,
                            offset..frames,
                            notification_tx,
                        )
                    }) else {
                        continue;
                    };
                    read_outcome = outcome;
                    skip_tracks[next_idx] = true;

                    if let Some(snapshot) = Self::outcome_position_duration(&read_outcome) {
                        leading_outcome_pos_dur = Some(snapshot);
                    }

                    handover_offset = Self::next_handover_offset(&read_outcome, offset);
                }

                if let Some(offset) = handover_offset
                    && offset < frames
                {
                    for (next_arena_idx, (next_handle, next_state)) in
                        arena_tracks.iter().enumerate()
                    {
                        if *next_state != TrackState::Preloading
                            || active_arena_slots[next_arena_idx]
                        {
                            continue;
                        }

                        let Some(next_track) = tracks.get_by_index_mut(*next_handle) else {
                            continue;
                        };
                        next_track.play();
                        let _ = next_track.read(
                            &mut read_bufs,
                            &mut mix_bufs,
                            offset..frames,
                            notification_tx,
                        );
                        break;
                    }
                }
            }

            for (out_ch, mix_ch) in buffers.outputs.iter_mut().zip(mix_bufs.iter()) {
                out_ch
                    .iter_mut()
                    .take(frames)
                    .zip(mix_ch.iter())
                    .for_each(|(out_sample, &mix_sample)| *out_sample += mix_sample);
            }
        }

        (playback_started, leading_outcome_pos_dur)
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
    fn unload_track(&mut self, src: &Arc<str>) {
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

/// Returns eviction priority for a track state (lower = evicted first).
fn eviction_priority(state: TrackState) -> u8 {
    /// Eviction priority: preloading tracks are evicted before active ones.
    const EVICT_PRELOADING: u8 = 2;
    /// Eviction priority: fading-in tracks are kept longer.
    const EVICT_FADING_IN: u8 = 3;
    /// Eviction priority: playing tracks are evicted last.
    const EVICT_PLAYING: u8 = 4;

    match state {
        TrackState::Finished => 0,
        TrackState::FadingOut => 1,
        TrackState::Preloading => EVICT_PRELOADING,
        TrackState::FadingIn => EVICT_FADING_IN,
        TrackState::Playing => EVICT_PLAYING,
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc as TestArc, Mutex,
        atomic::{AtomicU32, Ordering as AtomicOrdering},
    };

    use kithara_audio::PcmReader;
    use kithara_decode::{PcmSpec, TrackMetadata};
    use kithara_events::EventBus;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use ringbuf::traits::Producer;

    use super::*;
    use crate::{
        bridge::{SlotControl, slot_channels},
        impls::{resource::Resource, shared_eq::SharedEq},
    };

    fn stream_shape(sample_rate: NonZeroU32) -> StreamShape {
        StreamShape {
            sample_rate,
            max_block_frames: NonZeroU32::new(512).expect("BUG: non-zero"),
        }
    }

    fn make_processor() -> (PlayerNodeProcessor, SlotControl) {
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let sample_rate = NonZeroU32::new(44100).expect("BUG: non-zero");
        let processor =
            PlayerNodeProcessor::new(inputs, stream_shape(sample_rate), &PcmPool::default());
        (processor, control)
    }

    /// Reader that records the last `host_sample_rate` set via `set_host_sample_rate`.
    struct SampleRateTrackingReader {
        duration: Duration,
        bus: EventBus,
        spec: PcmSpec,
        recorded_host_rate: TestArc<AtomicU32>,
        meta: TrackMetadata,
    }

    impl SampleRateTrackingReader {
        fn new(spec: PcmSpec) -> (Self, TestArc<AtomicU32>) {
            Self::with_duration(spec, Duration::from_secs(60))
        }

        fn with_duration(spec: PcmSpec, duration: Duration) -> (Self, TestArc<AtomicU32>) {
            let recorded = TestArc::new(AtomicU32::new(0));
            let reader = Self {
                spec,
                duration,
                meta: TrackMetadata::default(),
                bus: EventBus::default(),
                recorded_host_rate: TestArc::clone(&recorded),
            };
            (reader, recorded)
        }
    }

    impl PcmReader for SampleRateTrackingReader {
        fn duration(&self) -> Option<Duration> {
            Some(self.duration)
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.meta
        }

        fn position(&self) -> Duration {
            Duration::ZERO
        }

        fn read(
            &mut self,
            _buf: &mut [f32],
        ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
            Ok(kithara_audio::ReadOutcome::Pending {
                reason: kithara_audio::PendingReason::Buffering,
                position: Duration::ZERO,
            })
        }

        fn read_planar<'a>(
            &mut self,
            _output: &'a mut [&'a mut [f32]],
        ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
            Ok(kithara_audio::ReadOutcome::Pending {
                reason: kithara_audio::PendingReason::Buffering,
                position: Duration::ZERO,
            })
        }

        fn seek(
            &mut self,
            position: Duration,
        ) -> Result<kithara_audio::SeekOutcome, kithara_audio::DecodeError> {
            Ok(kithara_audio::SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }

        fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
            self.recorded_host_rate
                .store(sample_rate.get(), AtomicOrdering::Relaxed);
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    #[kithara::test(tokio)]
    async fn load_track_propagates_host_sample_rate() {
        let host_rate = 88_200u32;

        let (reader, recorded) = SampleRateTrackingReader::new(PcmSpec::new(
            2,
            NonZeroU32::new(44100).expect("test rate"),
        ));

        let resource = Resource::from_reader(reader, None);
        let player_resource = Box::new(PlayerResource::new(
            resource,
            Arc::from("track.mp3"),
            &PcmPool::default(),
        ));

        let (inputs, mut control) = slot_channels(SharedEq::new(0));
        let sample_rate = NonZeroU32::new(host_rate).expect("BUG: non-zero");
        let mut processor =
            PlayerNodeProcessor::new(inputs, stream_shape(sample_rate), &PcmPool::default());

        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: player_resource,
                item_id: None,
                src: Arc::from("track.mp3"),
            })
            .ok();
        processor.drain_commands();

        assert_eq!(recorded.load(AtomicOrdering::Relaxed), host_rate);
    }

    #[kithara::test]
    fn processor_renders_silence_when_no_tracks() {
        let (processor, _control) = make_processor();
        assert_eq!(processor.tracks.len(), 0);
    }

    #[kithara::test]
    fn processor_seek_without_tracks_does_not_panic() {
        let (mut processor, mut control) = make_processor();
        control
            .cmd_tx
            .try_push(PlayerCmd::Seek {
                seconds: 30.0,
                seek_epoch: 1,
            })
            .ok();
        processor.drain_commands();
    }

    #[kithara::test]
    fn processor_set_playback_rate_without_tracks_does_not_panic() {
        let (mut processor, mut control) = make_processor();
        control
            .cmd_tx
            .try_push(PlayerCmd::SetPlaybackRate(2.0))
            .ok();
        processor.drain_commands();
    }

    #[kithara::test]
    fn processor_set_paused_updates_playback() {
        let (mut processor, mut control) = make_processor();

        control.cmd_tx.try_push(PlayerCmd::SetPaused(false)).ok();
        processor.drain_commands();
        assert!(processor.playback.playing.load(Ordering::SeqCst));

        control.cmd_tx.try_push(PlayerCmd::SetPaused(true)).ok();
        processor.drain_commands();
        assert!(!processor.playback.playing.load(Ordering::SeqCst));
    }

    #[kithara::test(tokio)]
    async fn processor_clear_unloads_tracks_and_resets_snapshot() {
        let (reader, _recorded) = SampleRateTrackingReader::new(PcmSpec::new(
            2,
            NonZeroU32::new(44100).expect("test rate"),
        ));
        let resource = Resource::from_reader(reader, None);
        let player_resource = Box::new(PlayerResource::new(
            resource,
            Arc::from("track.mp3"),
            &PcmPool::default(),
        ));

        let (mut processor, mut control) = make_processor();

        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: player_resource,
                item_id: None,
                src: Arc::from("track.mp3"),
            })
            .ok();
        processor.drain_commands();
        assert_eq!(processor.tracks.len(), 1);

        processor.playback.playing.store(true, Ordering::SeqCst);
        processor.playback.position.store(42.0, Ordering::Relaxed);
        processor.playback.duration.store(60.0, Ordering::Relaxed);

        control.cmd_tx.try_push(PlayerCmd::Clear).ok();
        processor.drain_commands();

        assert_eq!(processor.tracks.len(), 0, "arena must be empty after Clear");
        assert_eq!(processor.playback.position.load(Ordering::Relaxed), 0.0);
        assert_eq!(processor.playback.duration.load(Ordering::Relaxed), 0.0);
        assert!(!processor.playback.playing.load(Ordering::SeqCst));
    }

    fn create_duration_player_resource(src: &str, duration: Duration) -> Box<PlayerResource> {
        let (reader, _recorded) = SampleRateTrackingReader::with_duration(
            PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate")),
            duration,
        );
        let resource = Resource::from_reader(reader, None);
        Box::new(PlayerResource::new(
            resource,
            Arc::from(src),
            &PcmPool::default(),
        ))
    }

    #[kithara::test(tokio)]
    async fn fade_in_switches_public_snapshot_without_render() {
        let (mut processor, mut control) = make_processor();
        let first_src: Arc<str> = Arc::from("first.mp3");
        let second_src: Arc<str> = Arc::from("second.mp3");

        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: create_duration_player_resource(&first_src, Duration::from_secs(64)),
                item_id: None,
                src: Arc::clone(&first_src),
            })
            .ok();
        control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(Arc::clone(
                &first_src,
            ))))
            .ok();
        processor.drain_commands();

        assert_eq!(processor.playback.duration.load(Ordering::Relaxed), 64.0);

        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: create_duration_player_resource(&second_src, Duration::from_secs(162)),
                item_id: None,
                src: Arc::clone(&second_src),
            })
            .ok();
        processor.drain_commands();

        assert_eq!(
            processor.playback.duration.load(Ordering::Relaxed),
            64.0,
            "preload must not publish the next track duration"
        );

        control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(Arc::clone(
                &second_src,
            ))))
            .ok();
        processor.drain_commands();

        assert_eq!(processor.playback.position.load(Ordering::Relaxed), 0.0);
        assert_eq!(processor.playback.duration.load(Ordering::Relaxed), 162.0);
    }

    fn create_tracking_player_resource(
        src: &str,
        seek_log: TestArc<Mutex<Vec<u64>>>,
    ) -> Box<PlayerResource> {
        #[derive(Clone)]
        struct SeekTrackingReader {
            spec: PcmSpec,
            metadata: TrackMetadata,
            seek_log: TestArc<Mutex<Vec<u64>>>,
            bus: EventBus,
        }

        impl PcmReader for SeekTrackingReader {
            fn read(
                &mut self,
                _buf: &mut [f32],
            ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
                Ok(kithara_audio::ReadOutcome::Pending {
                    reason: kithara_audio::PendingReason::Buffering,
                    position: Duration::ZERO,
                })
            }

            fn read_planar<'a>(
                &mut self,
                output: &'a mut [&'a mut [f32]],
            ) -> Result<kithara_audio::ReadOutcome, kithara_audio::DecodeError> {
                let _ = output;
                Ok(kithara_audio::ReadOutcome::Pending {
                    reason: kithara_audio::PendingReason::Buffering,
                    position: Duration::ZERO,
                })
            }

            fn seek(
                &mut self,
                position: Duration,
            ) -> Result<kithara_audio::SeekOutcome, kithara_audio::DecodeError> {
                #[expect(clippy::cast_possible_truncation, reason = "test values fit in u64")]
                let ms = position.as_millis() as u64;
                self.seek_log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(ms);
                Ok(kithara_audio::SeekOutcome::Landed {
                    target: position,
                    landed_at: position,
                })
            }

            fn spec(&self) -> PcmSpec {
                self.spec
            }

            fn position(&self) -> Duration {
                Duration::ZERO
            }

            fn duration(&self) -> Option<Duration> {
                None
            }

            fn metadata(&self) -> &TrackMetadata {
                &self.metadata
            }

            fn event_bus(&self) -> &EventBus {
                &self.bus
            }
        }

        let reader = SeekTrackingReader {
            seek_log,
            spec: PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate")),
            metadata: TrackMetadata {
                title: Some("Tracking".to_owned()),
                ..Default::default()
            },
            bus: EventBus::default(),
        };

        let resource = Resource::from_reader(reader, None);
        Box::new(PlayerResource::new(
            resource,
            Arc::from(src),
            &PcmPool::default(),
        ))
    }

    #[kithara::test(tokio)]
    async fn processor_multiple_seek_epochs_only_last_applies() {
        let seek_log = TestArc::new(Mutex::new(Vec::new()));
        let resource = create_tracking_player_resource("track1.mp3", Arc::clone(&seek_log));

        let (mut processor, mut control) = make_processor();
        let src = Arc::from("track1.mp3");
        control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource,
                item_id: None,
                src: Arc::clone(&src),
            })
            .ok();
        processor.drain_commands();
        control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(Arc::clone(
                &src,
            ))))
            .ok();
        processor.drain_commands();

        let playback = Arc::clone(&processor.playback);
        let first = playback.next_seek_epoch();
        playback.seek_epoch.store(first, Ordering::SeqCst);
        control
            .cmd_tx
            .try_push(PlayerCmd::Seek {
                seconds: 10.0,
                seek_epoch: first,
            })
            .ok();
        let second = playback.next_seek_epoch();
        playback.seek_epoch.store(second, Ordering::SeqCst);
        control
            .cmd_tx
            .try_push(PlayerCmd::Seek {
                seconds: 20.0,
                seek_epoch: second,
            })
            .ok();
        let third = playback.next_seek_epoch();
        playback.seek_epoch.store(third, Ordering::SeqCst);
        control
            .cmd_tx
            .try_push(PlayerCmd::Seek {
                seconds: 30.0,
                seek_epoch: third,
            })
            .ok();

        processor.drain_commands();

        let recorded_seeks = {
            let seek_log = seek_log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            seek_log.clone()
        };
        assert_eq!(recorded_seeks, [30000]);
        assert_eq!(playback.seek_epoch.load(Ordering::SeqCst), third);
    }
}
