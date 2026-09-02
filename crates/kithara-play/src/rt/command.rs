use std::sync::atomic::Ordering;

use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use kithara_test_macros as kithara;
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
            // WHY: Slot-wide: the re-base releases the natural-end hold on every loaded track, including the ones this seek does not move.
            track.observe_seek_epoch(seek_epoch);
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
    #[kithara::measure]
    pub fn drain_commands(&mut self) {
        while let Some(cmd) = self.cmd_rx.try_pop() {
            match cmd {
                PlayerCmd::LoadTrack { resource, item_id } => {
                    self.load_track(resource, item_id);
                }
                PlayerCmd::UnloadTrack { item_id } => {
                    if let Some(slot) = self.tracks.slot_of(item_id) {
                        self.unload_slot(slot);
                    }
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
            }
        }
    }

    fn handle_transition(&mut self, transition: TrackTransition) {
        let mut leading_changed = false;

        if let TrackTransition::FadeIn(item_id) = &transition {
            self.tracks_transitions.clear();

            let maybe_old = self
                .tracks
                .iter()
                .find_map(|(_, track)| track.state().is_leading().then(|| track.item_id()));

            if let Some(old_id) = maybe_old
                && old_id != *item_id
            {
                leading_changed = true;
                self.tracks_transitions
                    .push_back(TrackTransition::FadeOut(old_id));
            }
        }

        self.tracks_transitions.push_back(transition);
        let playback = Arc::clone(&self.playback);
        let mut changed_src = None;
        self.tracks_transitions.retain(|transition| {
            let item_id = match transition {
                TrackTransition::FadeIn(item_id) | TrackTransition::FadeOut(item_id) => *item_id,
            };
            if let Some(track) = self.tracks.get_mut(item_id) {
                match transition {
                    TrackTransition::FadeIn(_) => {
                        changed_src = Some(Arc::clone(track.src()));
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

        if leading_changed && let Some(new_src) = changed_src {
            self.notif_tx
                .try_push(PlayerNotification::Changed { src: new_src })
                .ok();
        }
    }

    fn load_track(&mut self, resource: Box<PlayerResource>, item_id: TrackId) {
        let src = Arc::clone(resource.src());
        if let Some(slot) = self.tracks.slot_of(item_id) {
            self.unload_slot(slot);
        }
        self.evict_tracks_if_needed();

        resource.set_host_sample_rate(self.sample_rate);

        let track = PlayerTrack::builder()
            .sample_rate(self.sample_rate)
            .item_id(item_id)
            .fade_duration(self.crossfade.duration)
            .prefetch_duration(self.prefetch_duration)
            .fade_curve(self.crossfade.fade_curve())
            .seek_epoch(self.playback.seek_epoch.load(Ordering::SeqCst))
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
