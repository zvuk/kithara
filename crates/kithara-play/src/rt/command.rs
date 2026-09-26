use std::sync::atomic::Ordering;

use kithara_events::TrackId;
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
    }

    fn apply_prefetch_duration(&mut self, duration: f32) {
        self.prefetch_duration = duration.max(0.0);
        for (_, track) in self.tracks.iter_mut() {
            track.set_prefetch_duration(self.prefetch_duration);
        }
    }

    /// Releases the natural-end hold on every loaded track in the slot, including ones this seek
    /// does not move, since the re-base is slot-wide.
    fn apply_seek(&mut self, seconds: f64, seek_epoch: u64) {
        if seek_epoch != self.playback.seek_epoch.load(Ordering::SeqCst) {
            return;
        }

        let mut revived = false;
        for (_, track) in self.tracks.iter_mut() {
            track.observe_seek_epoch(seek_epoch);
            match track.state() {
                TrackState::FadingIn => {
                    track.seek(seconds);
                }
                TrackState::Playing => {
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

    fn clear_all_tracks(&mut self) -> bool {
        // A queued Clear keeps later commands behind it until every owned
        // staged object has off-RT custody. Muting first prevents a pending
        // ticket from claiming while return capacity is temporarily full.
        self.playback.playing.store(false, Ordering::SeqCst);
        self.playback.active_sync_map.store(0, Ordering::Release);
        for (_, track) in self.tracks.iter_mut() {
            track.stop();
        }
        let loaded: SmallVec<[TrackSlot; Self::MAX_TRACKS]> =
            self.tracks.iter().map(|(slot, _)| slot).collect();
        for slot in loaded {
            self.unload_slot(slot);
        }
        self.retire_sync_tail();
        self.retire_pending_sync(kithara_sync::SyncExecutionReject::Cancelled);
        self.tracks_transitions.clear();
        self.playback.position.store(0.0, Ordering::Relaxed);
        self.playback.frontier.store(0.0, Ordering::Relaxed);
        self.playback.cached.store(0.0, Ordering::Relaxed);
        self.playback.duration.store(0.0, Ordering::Relaxed);
        self.tracks.len() == 0 && self.sync_custody_cleared()
    }

    fn can_run_track_command(&self, command: &PlayerCmd) -> bool {
        match command {
            PlayerCmd::UnloadTrack { item_id } => self
                .tracks
                .get(*item_id)
                .is_none_or(|track| !track.has_sync_lane() || self.can_return_sync()),
            PlayerCmd::LoadTrack { item_id, .. } => {
                if self
                    .tracks
                    .get(*item_id)
                    .is_some_and(PlayerTrack::has_sync_lane)
                {
                    self.can_return_sync()
                } else if self.tracks.is_full() {
                    self.can_return_sync()
                        || self.tracks.iter().any(|(_, track)| !track.has_sync_lane())
                } else {
                    true
                }
            }
            _ => true,
        }
    }

    /// Drain all pending commands from the channel.
    pub fn drain_commands(&mut self) {
        while let Some(queued) = self.cmd_rx.try_peek() {
            if matches!(queued, PlayerCmd::Clear) {
                if !self.clear_all_tracks() {
                    break;
                }
                let _ = self.cmd_rx.try_pop();
                continue;
            }
            if !self.can_run_track_command(queued) {
                break;
            }
            let Some(cmd) = self.cmd_rx.try_pop() else {
                unreachable!("sole command consumer lost a peeked command");
            };
            match cmd {
                PlayerCmd::LoadTrack {
                    resource,
                    item_id,
                    load,
                } => {
                    self.load_track(resource, item_id, load);
                }
                PlayerCmd::UnloadTrack { item_id } => {
                    if let Some(slot) = self.tracks.slot_of(item_id) {
                        self.unload_slot(slot);
                    }
                }
                PlayerCmd::Clear => {
                    unreachable!("Clear is completed before leaving the command ring");
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
        let selected = self.playback.active_sync_map.load(Ordering::Relaxed);
        let selected_item = self.tracks.iter().find_map(|(_, track)| {
            track
                .sync_map()
                .is_some_and(|map| u64::from(map) == selected)
                .then_some(track.item_id())
        });

        if let TrackTransition::FadeIn { item_id, settings } = &transition {
            self.tracks_transitions.clear();

            let maybe_old = self
                .tracks
                .iter()
                .find_map(|(_, track)| track.state().is_leading().then(|| track.item_id()));

            if let Some(old_id) = maybe_old
                && old_id != *item_id
            {
                leading_changed = true;
                self.tracks_transitions.push_back(TrackTransition::FadeOut {
                    item_id: old_id,
                    settings: *settings,
                });
            }
        }

        self.tracks_transitions.push_back(transition);
        let playback = Arc::clone(&self.playback);
        let mut changed_src = None;
        self.tracks_transitions.retain(|transition| {
            let item_id = match transition {
                TrackTransition::FadeIn { item_id, .. }
                | TrackTransition::FadeOut { item_id, .. } => *item_id,
            };
            if let Some(track) = self.tracks.get_mut(item_id) {
                match transition {
                    TrackTransition::FadeIn { settings, .. } => {
                        changed_src = Some(Arc::clone(track.src()));
                        if track.position() > Self::FADE_IN_SEEK_THRESHOLD {
                            track.seek(0.0);
                        }
                        track.fade_in(*settings);
                        if selected_item.is_some_and(|selected| selected != item_id) {
                            playback.active_sync_map.store(0, Ordering::Release);
                        }
                        playback.position.store(track.position(), Ordering::Relaxed);
                        playback.duration.store(track.duration(), Ordering::Relaxed);
                    }
                    TrackTransition::FadeOut { settings, .. } => {
                        track.fade_out(*settings);
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

    fn load_track(
        &mut self,
        resource: Box<PlayerResource>,
        item_id: TrackId,
        load: kithara_sync::LoadGeneration,
    ) {
        let src = Arc::clone(resource.src());
        if let Some(slot) = self.tracks.slot_of(item_id) {
            self.unload_slot(slot);
        }
        self.evict_tracks_if_needed();

        resource.set_host_sample_rate(self.sample_rate);

        let track = PlayerTrack::builder()
            .sample_rate(self.sample_rate)
            .item_id(item_id)
            .load(load)
            .crossfade(self.crossfade)
            .prefetch_duration(self.prefetch_duration)
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
