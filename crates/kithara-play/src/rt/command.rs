use std::sync::atomic::Ordering;

use kithara_platform::sync::Arc;
use ringbuf::traits::{Consumer, Producer};
use smallvec::SmallVec;

use super::{
    processor::PlayerNodeProcessor,
    track::{PlayerResource, PlayerTrack, TrackParams},
};
use crate::bridge::{PlayerCmd, PlayerNotification, TrackState, TrackTransition};

impl PlayerNodeProcessor {
    fn apply_fade_duration(&mut self, duration: f32) {
        self.crossfade.duration = duration;
        for (_, track) in self.tracks.iter_mut() {
            track.update_fade_duration(duration, self.sample_rate);
        }
    }

    fn apply_prefetch_duration(&mut self, duration: f32) {
        self.prefetch_duration = duration.max(0.0);
        for (_, track) in self.tracks.iter_mut() {
            track.set_prefetch_duration(self.prefetch_duration);
        }
    }

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
                PlayerCmd::LoadTrack { resource, item_id } => {
                    self.load_track(resource, item_id);
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
                PlayerCmd::SetPitchBend(bend) => {
                    self.tracks
                        .iter()
                        .for_each(|(_, track)| track.resource().set_transport_bend(bend));
                }
            }
        }
    }

    fn handle_transition(&mut self, transition: TrackTransition) {
        let (mut old_track, mut new_track) = (None, None);

        if let TrackTransition::FadeIn(ref nt) = transition {
            new_track = Some(nt.clone());
            self.tracks_transitions.clear();

            let maybe_old = self.tracks.iter_keys().find_map(|(key, idx)| {
                self.tracks
                    .get_by_index(*idx)
                    .filter(|t| t.state().is_leading())
                    .map(|_| key.clone())
            });

            if let Some(ref ot) = maybe_old
                && ot != nt
            {
                old_track = Some(ot.clone());
                self.tracks_transitions
                    .push_back(TrackTransition::FadeOut(ot.clone()));
            }
        }

        self.tracks_transitions.push_back(transition);
        let playback = Arc::clone(&self.playback);
        self.tracks_transitions.retain(|transition| {
            let track_src = match transition {
                TrackTransition::FadeIn(src) | TrackTransition::FadeOut(src) => src.clone(),
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

    fn load_track(&mut self, resource: Box<PlayerResource>, item_id: Option<Arc<str>>) {
        let src = Arc::clone(resource.src());
        if let Some(track) = self.tracks.remove(&src) {
            self.discard_track(track);
            self.notif_tx
                .try_push(PlayerNotification::Unloaded { src: src.clone() })
                .ok();
        }

        self.evict_tracks_if_needed();

        resource.set_host_sample_rate(self.sample_rate);

        let loaded_src = src.clone();
        let params = TrackParams::builder()
            .src(src.clone())
            .sample_rate(self.sample_rate)
            .maybe_item_id(item_id)
            .fade_duration(self.crossfade.duration)
            .prefetch_duration(self.prefetch_duration)
            .fade_curve(self.crossfade.fade_curve())
            .build();
        let track = PlayerTrack::new(resource, params);
        self.tracks.insert(src, track);

        self.notif_tx
            .try_push(PlayerNotification::Loaded { src: loaded_src })
            .ok();
    }
}
