//! Realtime audio processor for the player node.
//!
//! [`PlayerNodeProcessor`] runs on the audio thread and manages multiple
//! [`PlayerTrack`]s in a thunderdome arena. It receives commands from the
//! main thread via a kanal channel and renders mixed audio into the
//! Firewheel output buffers.

use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
};

use firewheel::{
    StreamInfo,
    event::ProcEvents,
    node::{AudioNodeProcessor, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus},
};
use kithara_bufpool::{PcmBuf, pcm_pool};
use kithara_platform::Mutex;
use thunderdome::{Arena, Index};
use tracing::warn;

use crate::traits::dj::crossfade::CrossfadeCurve;

use super::{
    crossfade::CrossfadeSettings,
    player_notification::PlayerNotification,
    player_resource::PlayerResource,
    player_track::{PlayerTrack, TrackState, TrackTransition},
    shared_player_state::SharedPlayerState,
};

/// Maximum number of concurrent tracks per player node.
const MAX_TRACKS: usize = 4;

/// Commands sent from the main thread to the processor.
#[expect(
    dead_code,
    reason = "some variants used when seek/unload/crossfade are wired"
)]
pub(crate) enum PlayerCmd {
    /// Load a track into the processor arena.
    LoadTrack {
        resource: Arc<Mutex<PlayerResource>>,
        src: Arc<str>,
    },
    /// Unload a track by its source identifier.
    UnloadTrack { src: Arc<str> },
    /// Add a track transition (fade in / fade out).
    Transition(TrackTransition),
    /// Seek active tracks to the given position in seconds.
    Seek(f64),
    /// Set the paused state.
    SetPaused(bool),
    /// Update the fade duration.
    SetFadeDuration(f32),
    /// Update the crossfade curve.
    SetCrossfadeCurve(CrossfadeCurve),
}

impl std::fmt::Debug for PlayerCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadTrack { src, .. } => f.debug_struct("LoadTrack").field("src", src).finish(),
            Self::UnloadTrack { src } => f.debug_struct("UnloadTrack").field("src", src).finish(),
            Self::Transition(t) => f.debug_tuple("Transition").field(t).finish(),
            Self::Seek(s) => f.debug_tuple("Seek").field(s).finish(),
            Self::SetPaused(p) => f.debug_tuple("SetPaused").field(p).finish(),
            Self::SetFadeDuration(d) => f.debug_tuple("SetFadeDuration").field(d).finish(),
            Self::SetCrossfadeCurve(c) => f.debug_tuple("SetCrossfadeCurve").field(c).finish(),
        }
    }
}

/// The realtime audio processor for the player node.
///
/// Manages tracks in a thunderdome arena, handles transitions,
/// and renders mixed stereo audio into the Firewheel output buffers.
pub(crate) struct PlayerNodeProcessor {
    cmd_rx: kanal::Receiver<PlayerCmd>,
    shared_state: Arc<SharedPlayerState>,
    tracks: Arena<PlayerTrack>,
    tracks_index: HashMap<Arc<str>, Index>,
    tracks_transitions: VecDeque<TrackTransition>,
    crossfade: CrossfadeSettings,
    sample_rate: NonZeroU32,
    scratch_bufs: [PcmBuf; 4],
}

impl PlayerNodeProcessor {
    /// Create a new processor with the given command receiver and shared state.
    pub(crate) fn new(
        cmd_rx: kanal::Receiver<PlayerCmd>,
        shared_state: Arc<SharedPlayerState>,
        sample_rate: NonZeroU32,
    ) -> Self {
        let scratch_bufs = [
            pcm_pool().get(),
            pcm_pool().get(),
            pcm_pool().get(),
            pcm_pool().get(),
        ];

        Self {
            cmd_rx,
            shared_state,
            tracks: Arena::with_capacity(MAX_TRACKS),
            tracks_index: HashMap::with_capacity(MAX_TRACKS),
            tracks_transitions: VecDeque::with_capacity(MAX_TRACKS),
            crossfade: CrossfadeSettings::default(),
            sample_rate,
            scratch_bufs,
        }
    }

    /// Drain all pending commands from the channel.
    fn drain_commands(&mut self) {
        while let Ok(Some(cmd)) = self.cmd_rx.try_recv() {
            match cmd {
                PlayerCmd::LoadTrack { resource, src } => {
                    self.load_track(resource, src);
                }
                PlayerCmd::UnloadTrack { src } => {
                    self.unload_track(&src);
                }
                PlayerCmd::Transition(transition) => {
                    self.handle_transition(transition);
                }
                PlayerCmd::Seek(seconds) => {
                    self.apply_seek(seconds);
                }
                PlayerCmd::SetPaused(paused) => {
                    let playing = !paused;
                    self.shared_state.playing.store(playing, Ordering::SeqCst);
                }
                PlayerCmd::SetFadeDuration(duration) => {
                    self.apply_fade_duration(duration);
                }
                PlayerCmd::SetCrossfadeCurve(curve) => {
                    self.crossfade.curve = curve;
                    let fade_curve = self.crossfade.fade_curve();
                    for (_, track) in &mut self.tracks {
                        track.set_fade_curve(fade_curve);
                    }
                }
            }
        }
    }

    /// Load a new track into the arena.
    fn load_track(&mut self, resource: Arc<Mutex<PlayerResource>>, src: Arc<str>) {
        if self.tracks_index.contains_key(&src) {
            return; // Already loaded
        }

        self.evict_tracks_if_needed();

        let track = PlayerTrack::new(
            resource,
            Arc::clone(&src),
            self.crossfade.duration,
            self.sample_rate,
            self.crossfade.fade_curve(),
        );
        let idx = self.tracks.insert(track);
        self.tracks_index.insert(Arc::clone(&src), idx);

        self.shared_state
            .notification_tx
            .try_send(PlayerNotification::TrackLoaded(src))
            .ok();
    }

    /// Unload a track from the arena.
    fn unload_track(&mut self, src: &Arc<str>) {
        if let Some(idx) = self.tracks_index.remove(src) {
            self.tracks.remove(idx);
            self.shared_state
                .notification_tx
                .try_send(PlayerNotification::TrackUnloaded(Arc::clone(src)))
                .ok();
        }
    }

    /// Handle a track transition command.
    fn handle_transition(&mut self, transition: TrackTransition) {
        let (mut old_track, mut new_track) = (None, None);

        if let TrackTransition::FadeIn(ref nt) = transition {
            new_track = Some(Arc::clone(nt));

            // Clear pending transitions from previous requests
            self.tracks_transitions.clear();

            // Find current leading track for automatic fade-out
            let maybe_old = self.tracks_index.iter().find_map(|(key, &idx)| {
                self.tracks
                    .get(idx)
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

        // Process the transition queue
        self.tracks_transitions.retain(|transition| {
            let (track_src, action): (_, fn(&mut PlayerTrack)) = match transition {
                TrackTransition::FadeIn(src) => (Arc::clone(src), PlayerTrack::fade_in),
                TrackTransition::FadeOut(src) => (Arc::clone(src), PlayerTrack::fade_out),
            };
            if let Some(track_index) = self.tracks_index.get(&track_src)
                && let Some(track) = self.tracks.get_mut(*track_index)
            {
                (action)(track);
                return false; // Applied, remove from queue
            }
            true // Track not found, keep in queue for retry
        });

        // Notify about track change
        if let (Some(old), Some(new)) = (old_track, new_track) {
            self.shared_state
                .notification_tx
                .try_send(PlayerNotification::TrackChanged { old, new })
                .ok();
        }
    }

    /// Evict tracks to make room when at capacity.
    ///
    /// Tracks are evicted in priority order: `Finished` first, then `FadingOut`,
    /// `Preloading`, `Paused`, `FadingIn`, and `Playing` last. If all tracks are in the
    /// same state (e.g. all Playing), eviction order is non-deterministic
    /// because `HashMap` iteration order is undefined.
    fn evict_tracks_if_needed(&mut self) {
        while self.tracks_index.len() >= MAX_TRACKS {
            let eviction_candidate = self
                .tracks_index
                .iter()
                .min_by_key(|(_, idx)| {
                    self.tracks.get(**idx).map_or(0, |t| match t.state() {
                        TrackState::Finished => 0,
                        TrackState::FadingOut => 1,
                        TrackState::Preloading => 2,
                        TrackState::Paused => 3,
                        TrackState::FadingIn => 4,
                        TrackState::Playing => 5,
                    })
                })
                .map(|(key, idx)| {
                    let state = self.tracks.get(*idx).map(PlayerTrack::state);
                    (Arc::clone(key), state)
                });

            if let Some((key, state)) = eviction_candidate {
                if state == Some(TrackState::Playing) {
                    warn!(
                        src = &*key,
                        "evicting a Playing track to make room for a new track"
                    );
                }
                if let Some(idx) = self.tracks_index.remove(&key) {
                    self.tracks.remove(idx);
                    self.shared_state
                        .notification_tx
                        .try_send(PlayerNotification::TrackUnloaded(key))
                        .ok();
                }
            } else {
                break;
            }
        }
    }

    /// Apply seek to active tracks.
    fn apply_seek(&mut self, seconds: f64) {
        for (_, track) in &mut self.tracks {
            match track.state() {
                TrackState::FadingIn | TrackState::Playing => {
                    track.seek(seconds);
                    track.play();
                }
                TrackState::FadingOut => {
                    track.stop();
                }
                _ => {}
            }
        }
    }

    /// Update fade duration for all tracks.
    fn apply_fade_duration(&mut self, duration: f32) {
        self.crossfade.duration = duration;
        for (_, track) in &mut self.tracks {
            track.update_fade_duration(duration, self.sample_rate);
        }
    }

    /// Clean up finished tracks.
    ///
    /// Uses a stack-allocated array instead of `Vec` since `MAX_TRACKS` is 4,
    /// avoiding heap allocation on every `process()` call.
    fn cleanup_finished_tracks(&mut self) {
        let mut finished_indices: [Option<Index>; MAX_TRACKS] = [None; MAX_TRACKS];
        let mut count = 0;

        for (idx, track) in &self.tracks {
            if track.state() == TrackState::Finished && count < MAX_TRACKS {
                finished_indices[count] = Some(idx);
                count += 1;
            }
        }

        for idx in finished_indices[..count].iter().flatten() {
            if let Some(track) = self.tracks.remove(*idx) {
                let src = Arc::clone(track.src());
                self.tracks_index.retain(|_, v| *v != *idx);
                self.shared_state
                    .notification_tx
                    .try_send(PlayerNotification::TrackUnloaded(src))
                    .ok();
            }
        }
    }

    /// Update position and duration from the leading track.
    fn update_position_duration(&self) {
        for (_, track) in &self.tracks {
            if track.state().is_leading() {
                self.shared_state
                    .position
                    .store(track.position(), Ordering::Relaxed);
                self.shared_state
                    .duration
                    .store(track.duration(), Ordering::Relaxed);
                break;
            }
        }
    }

    /// Render audio for all active tracks into the output buffers.
    fn render_audio(&mut self, buffers: &mut ProcBuffers, frames: usize, is_playing: bool) -> bool {
        let mut playback_started = false;

        if buffers.outputs.len() < 2 {
            return false;
        }

        // Clear main output buffers
        for ch_buffer in buffers.outputs.iter_mut() {
            ch_buffer[..frames].fill(0.0);
        }

        // Resize scratch buffers
        for buf in &mut self.scratch_bufs {
            let cap = buf.capacity();
            if cap < frames {
                buf.reserve(frames - cap);
            }
            buf.resize(frames, 0.0);
        }

        // Split into read_bufs and mix_bufs
        let (left, right) = self.scratch_bufs.split_at_mut(2);
        let (read_buf0, read_buf1) = left.split_at_mut(1);
        let (mix_buf0, mix_buf1) = right.split_at_mut(1);
        let mut read_bufs = [&mut read_buf0[0][..frames], &mut read_buf1[0][..frames]];
        let mut mix_bufs = [&mut mix_buf0[0][..frames], &mut mix_buf1[0][..frames]];

        for (_, track) in &mut self.tracks {
            if is_playing && track.state().is_playing() {
                // Clear mix_bufs (wet signal) before each track
                for ch_buffer in &mut mix_bufs {
                    ch_buffer.fill(0.0);
                }

                track.read(
                    &mut read_bufs,
                    &mut mix_bufs,
                    0..frames,
                    &self.shared_state.notification_tx,
                );

                // Additively mix into main output
                for (out_ch, mix_ch) in buffers.outputs.iter_mut().zip(mix_bufs.iter()) {
                    for (out_sample, &mix_sample) in
                        out_ch.iter_mut().zip(mix_ch.iter()).take(frames)
                    {
                        *out_sample += mix_sample;
                    }
                }
                playback_started = true;
            }
        }

        playback_started
    }
}

impl AudioNodeProcessor for PlayerNodeProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        let new_sr = stream_info.sample_rate;
        self.sample_rate = new_sr;
        self.shared_state
            .sample_rate
            .store(new_sr.get(), Ordering::Relaxed);

        // Update host_sample_rate for all active tracks
        for (_, track) in &self.tracks {
            if let Some(resource) = track.resource().try_lock() {
                resource.set_host_sample_rate(new_sr);
            }
        }
    }

    fn process(
        &mut self,
        info: &ProcInfo,
        mut buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        // 1. Drain commands
        self.drain_commands();

        // 2. Cleanup finished tracks
        self.cleanup_finished_tracks();

        // 3. Get current playing state
        let is_playing = self.shared_state.playing.load(Ordering::SeqCst);

        // 4. Render audio
        let playback_started = self.render_audio(&mut buffers, info.frames, is_playing);

        // 5. Update position/duration
        self.update_position_duration();

        if playback_started {
            ProcessStatus::OutputsModified
        } else {
            ProcessStatus::ClearAllOutputs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shared_state() -> Arc<SharedPlayerState> {
        Arc::new(SharedPlayerState::new())
    }

    fn make_processor() -> (PlayerNodeProcessor, kanal::Sender<PlayerCmd>) {
        let shared_state = make_shared_state();
        let (tx, rx) = kanal::bounded(32);
        let sample_rate = NonZeroU32::new(44100).expect("non-zero");
        let processor = PlayerNodeProcessor::new(rx, shared_state, sample_rate);
        (processor, tx)
    }

    #[test]
    fn processor_renders_silence_when_no_tracks() {
        let (processor, _tx) = make_processor();
        assert_eq!(processor.tracks.len(), 0);
        assert_eq!(processor.tracks_index.len(), 0);
    }

    #[tokio::test]
    async fn processor_loads_track_via_command() {
        let (mut processor, tx) = make_processor();

        let resource = create_mock_player_resource("track1.mp3");
        tx.send(PlayerCmd::LoadTrack {
            resource,
            src: Arc::from("track1.mp3"),
        })
        .ok();

        processor.drain_commands();

        assert_eq!(processor.tracks.len(), 1);
        assert!(processor.tracks_index.contains_key("track1.mp3"));
    }

    #[tokio::test]
    async fn processor_unloads_track_via_command() {
        let (mut processor, tx) = make_processor();

        let resource = create_mock_player_resource("track1.mp3");
        tx.send(PlayerCmd::LoadTrack {
            resource,
            src: Arc::from("track1.mp3"),
        })
        .ok();
        processor.drain_commands();
        assert_eq!(processor.tracks.len(), 1);

        tx.send(PlayerCmd::UnloadTrack {
            src: Arc::from("track1.mp3"),
        })
        .ok();
        processor.drain_commands();
        assert_eq!(processor.tracks.len(), 0);
    }

    #[test]
    fn processor_seek_without_tracks_does_not_panic() {
        let (mut processor, tx) = make_processor();
        tx.send(PlayerCmd::Seek(30.0)).ok();
        processor.drain_commands();
    }

    #[test]
    fn processor_set_paused_updates_shared_state() {
        let (mut processor, tx) = make_processor();

        tx.send(PlayerCmd::SetPaused(false)).ok();
        processor.drain_commands();
        assert!(processor.shared_state.playing.load(Ordering::SeqCst));

        tx.send(PlayerCmd::SetPaused(true)).ok();
        processor.drain_commands();
        assert!(!processor.shared_state.playing.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn processor_duplicate_load_is_noop() {
        let (mut processor, tx) = make_processor();

        let resource1 = create_mock_player_resource("track1.mp3");
        let resource2 = create_mock_player_resource("track1.mp3");

        tx.send(PlayerCmd::LoadTrack {
            resource: resource1,
            src: Arc::from("track1.mp3"),
        })
        .ok();
        tx.send(PlayerCmd::LoadTrack {
            resource: resource2,
            src: Arc::from("track1.mp3"),
        })
        .ok();
        processor.drain_commands();

        assert_eq!(processor.tracks.len(), 1);
    }

    #[tokio::test]
    async fn processor_cleanup_finished_tracks() {
        let (mut processor, tx) = make_processor();

        let resource = create_mock_player_resource("track1.mp3");
        tx.send(PlayerCmd::LoadTrack {
            resource,
            src: Arc::from("track1.mp3"),
        })
        .ok();
        processor.drain_commands();

        // Manually set state to finished
        let key: Arc<str> = Arc::from("track1.mp3");
        if let Some(idx) = processor.tracks_index.get(&key)
            && let Some(track) = processor.tracks.get_mut(*idx)
        {
            track.stop();
        }

        processor.cleanup_finished_tracks();
        assert_eq!(processor.tracks.len(), 0);
    }

    fn create_mock_player_resource(src: &str) -> Arc<Mutex<PlayerResource>> {
        use std::time::Duration;

        use kithara_audio::PcmReader;
        use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
        use kithara_events::AudioEvent;
        use tokio::sync::broadcast;

        use crate::impls::resource::Resource;

        struct MockReader {
            spec: PcmSpec,
            metadata: TrackMetadata,
            events_tx: broadcast::Sender<AudioEvent>,
        }

        impl PcmReader for MockReader {
            fn read(&mut self, buf: &mut [f32]) -> usize {
                buf.fill(0.5);
                buf.len()
            }

            fn read_planar<'a>(&mut self, output: &'a mut [&'a mut [f32]]) -> usize {
                if output.is_empty() {
                    return 0;
                }
                let frames = output[0].len();
                for ch in output.iter_mut() {
                    ch.fill(0.5);
                }
                frames
            }

            fn seek(&mut self, _position: Duration) -> DecodeResult<()> {
                Ok(())
            }

            fn spec(&self) -> PcmSpec {
                self.spec
            }

            fn is_eof(&self) -> bool {
                false
            }

            fn position(&self) -> Duration {
                Duration::ZERO
            }

            fn duration(&self) -> Option<Duration> {
                Some(Duration::from_secs(60))
            }

            fn metadata(&self) -> &TrackMetadata {
                &self.metadata
            }

            fn decode_events(&self) -> broadcast::Receiver<AudioEvent> {
                self.events_tx.subscribe()
            }
        }

        let (events_tx, _) = broadcast::channel(64);
        let reader = MockReader {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            metadata: TrackMetadata {
                album: None,
                artist: None,
                artwork: None,
                title: Some("Mock".to_owned()),
            },
            events_tx,
        };

        let resource = Resource::from_reader(reader);
        Arc::new(Mutex::new(PlayerResource::new(resource, Arc::from(src))))
    }
}
