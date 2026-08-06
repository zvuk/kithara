use std::sync::atomic::Ordering;

use kithara_platform::sync::Arc;
use ringbuf::traits::{Consumer, Producer};
use smallvec::SmallVec;

use super::{
    TrackSlot,
    processor::PlayerNodeProcessor,
    track::{PlayerResource, PlayerTrack},
};
use crate::bridge::{PlayerCmd, PlayerNotification, TrackState, TrackTransition};

impl PlayerNodeProcessor {
    fn apply_fade_duration(&mut self, duration: f32) {
        self.crossfade.duration = duration;
        for (_, track) in self.tracks.iter_mut() {
            track.update_fade_duration(duration, self.sample_rate);
        }
    }

    fn apply_playback_rate(&mut self, rate: f32) {
        self.playback_rate = rate;
        for (_, track) in self.tracks.iter_mut() {
            track.set_playback_rate(rate);
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
        let loaded: SmallVec<[TrackSlot; Self::MAX_TRACKS]> =
            self.tracks.iter().map(|(slot, _)| slot).collect();
        for slot in loaded {
            self.unload_slot(slot);
        }
        self.tracks_transitions.clear();
        self.playback.playing.store(false, Ordering::SeqCst);
        self.playback.position.store(0.0, Ordering::Relaxed);
        self.playback.frontier.store(0.0, Ordering::Relaxed);
        self.playback.cached.store(0.0, Ordering::Relaxed);
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
                    self.apply_playback_rate(rate);
                }
            }
        }
    }

    fn handle_transition(&mut self, transition: TrackTransition) {
        let (mut old_track, mut new_track) = (None, None);

        if let TrackTransition::FadeIn(ref nt) = transition {
            new_track = Some(nt.clone());
            self.tracks_transitions.clear();

            let maybe_old = self
                .tracks
                .iter()
                .find_map(|(_, track)| track.state().is_leading().then(|| Arc::clone(track.src())));

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
        self.unload_track(&src);
        self.evict_tracks_if_needed();

        resource.set_host_sample_rate(self.sample_rate);

        let track = PlayerTrack::builder()
            .sample_rate(self.sample_rate)
            .maybe_item_id(item_id)
            .fade_duration(self.crossfade.duration)
            .prefetch_duration(self.prefetch_duration)
            .fade_curve(self.crossfade.fade_curve())
            .playback_rate(self.playback_rate)
            .build(resource);

        if let Some(rejected) = self.tracks.insert(track) {
            self.discard_track(rejected);
            return;
        }

        self.notif_tx
            .try_push(PlayerNotification::Loaded { src })
            .ok();
    }
}
