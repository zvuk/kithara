use std::sync::atomic::Ordering;

use kithara_platform::sync::Arc;
use ringbuf::traits::{Consumer, Producer};
use smallvec::SmallVec;

use super::processor::{PendingSessionSeek, PlayerNodeProcessor};
use crate::{
    api::{SessionBeat, Tempo, TrackBinding, TransportRevision},
    bridge::{
        PlayerCmd, PlayerNotification, SessionSeekAttempt, TrackStart, TrackState, TrackTransition,
    },
    player::track::{PlayerResource, PlayerTrack, TrackAxis, TrackParams},
};

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
                    if track.seek(seconds) {
                        track.play();
                    }
                }
                TrackState::FadingOut => {
                    track.stop();
                }
                TrackState::Finished
                    if track.ended_at_eof()
                        && seconds < track.duration()
                        && track.seek(seconds) =>
                {
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

    fn apply_start(&mut self, start: TrackStart) {
        let candidate = self
            .tracks
            .iter()
            .find(|(_, track)| track.state().is_leading())
            .map(|(index, _)| index)
            .or_else(|| {
                self.tracks
                    .iter()
                    .find(|(_, track)| track.state() == TrackState::Preloading)
                    .map(|(index, _)| index)
            });
        let Some(candidate) = candidate else {
            return;
        };
        let Some(track) = self.tracks.get_by_index_mut(candidate) else {
            return;
        };
        match start {
            TrackStart::Immediate => track.play(),
            scheduled @ TrackStart::Session { .. } => track.schedule_start(scheduled),
        }
        self.playback.playing.store(true, Ordering::SeqCst);
    }

    fn begin_session_seek(
        &mut self,
        attempt: SessionSeekAttempt,
        target: SessionBeat,
        tempo: Tempo,
        revision: TransportRevision,
    ) {
        if let Some(pending) = self.session_seek {
            self.fail_session_seek(pending.attempt);
        }
        self.session_seek = Some(PendingSessionSeek {
            target,
            attempt,
            revision,
        });
        let leading_count = self
            .tracks
            .iter()
            .filter(|(_, track)| track.state().is_leading())
            .count();
        if leading_count != 1 {
            self.fail_session_seek(attempt);
            return;
        }
        let prepared = self
            .tracks
            .iter_mut()
            .find(|(_, track)| track.state().is_leading())
            .is_some_and(|(_, track)| track.begin_session_seek(target, tempo, revision).is_ok());
        if !prepared {
            self.fail_session_seek(attempt);
        }
    }

    fn cancel_session_seek(&mut self, pending: PendingSessionSeek) {
        for (_, track) in self.tracks.iter_mut() {
            track.cancel_session_seek(pending.revision);
        }
        if self
            .session_seek
            .is_some_and(|current| current.attempt == pending.attempt)
        {
            self.session_seek = None;
        }
        self.playback.clear_prepared_session_seek(pending.attempt);
        self.playback
            .acknowledge_session_seek_cancel(pending.attempt);
    }

    fn clear_all_tracks(&mut self) {
        self.fail_pending_session_seek();
        for (_, track) in self.tracks.iter_mut() {
            track.stop();
        }
        let keys: SmallVec<[Arc<str>; Self::MAX_TRACKS]> = self
            .tracks
            .iter_keys()
            .map(|(key, _)| Arc::clone(key))
            .collect();
        for key in keys {
            if self.retirement_blocked() {
                break;
            }
            self.unload_track(&key);
        }
        self.tracks_transitions.clear();
        self.playback.playing.store(false, Ordering::SeqCst);
        self.playback.position.store(0.0, Ordering::Relaxed);
        self.playback.frontier.store(0.0, Ordering::Relaxed);
        self.playback.duration.store(0.0, Ordering::Relaxed);
        self.playback.multiple_tracks.store(false, Ordering::SeqCst);
    }

    /// Drain all pending commands from the channel.
    pub fn drain_commands(&mut self) {
        while !self.retirement_blocked() {
            let Some(cmd) = self.cmd_rx.try_pop() else {
                break;
            };
            match cmd {
                PlayerCmd::LoadTrack {
                    binding,
                    resource,
                    item_id,
                    start,
                } => {
                    self.fail_pending_session_seek();
                    self.load_track(resource, item_id, binding, start);
                }
                PlayerCmd::UnloadTrack { src } => {
                    self.fail_pending_session_seek();
                    self.unload_track(&src);
                }
                PlayerCmd::Clear => {
                    self.clear_all_tracks();
                }
                PlayerCmd::Transition(transition) => {
                    self.fail_pending_session_seek();
                    self.handle_transition(transition);
                }
                PlayerCmd::Seek {
                    seconds,
                    seek_epoch,
                } => {
                    self.apply_seek(seconds, seek_epoch);
                }
                PlayerCmd::StartAt(start) => {
                    self.apply_start(start);
                }
                PlayerCmd::PrepareSessionSeek {
                    attempt,
                    target,
                    tempo,
                    revision,
                } => {
                    self.begin_session_seek(attempt, target, tempo, revision);
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
                        .for_each(|(_, track)| track.set_playback_rate(rate));
                }
                PlayerCmd::SetPitchBend(bend) => {
                    self.tracks
                        .iter()
                        .for_each(|(_, track)| track.set_transport_bend(bend));
                }
            }
        }
    }

    fn fail_pending_session_seek(&mut self) {
        if let Some(pending) = self.session_seek {
            self.fail_session_seek(pending.attempt);
        }
    }

    pub(super) fn fail_session_seek(&mut self, attempt: SessionSeekAttempt) {
        if let Some(pending) = self
            .session_seek
            .filter(|pending| pending.attempt == attempt)
        {
            self.cancel_session_seek(pending);
        }
        self.playback.fail_session_seek(attempt);
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
                            let _ = track.seek(0.0);
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

    fn load_track(
        &mut self,
        resource: PlayerResource,
        item_id: Option<Arc<str>>,
        binding: Option<TrackBinding>,
        start: TrackStart,
    ) {
        let src = Arc::clone(resource.src());
        if let Some(track) = self.tracks.remove(&src) {
            self.discard_track(track);
            self.notif_tx
                .try_push(PlayerNotification::Unloaded { src: src.clone() })
                .ok();
        }

        self.evict_tracks_if_needed();

        let loaded_src = src.clone();
        let binding_event = binding.as_ref().map(|binding| {
            (
                binding.direction(),
                binding.session_anchor().get(),
                binding.track_anchor().get(),
            )
        });
        let axis = binding.map_or_else(|| TrackAxis::from(self.sample_rate), TrackAxis::from);
        let params = TrackParams::builder()
            .axis(axis)
            .src(src.clone())
            .maybe_item_id(item_id)
            .fade_duration(self.crossfade.duration)
            .prefetch_duration(self.prefetch_duration)
            .fade_curve(self.crossfade.fade_curve())
            .build();
        let scheduled = matches!(start, TrackStart::Session { .. });
        let mut track = PlayerTrack::new(resource, params);
        track.schedule_start(start);
        self.tracks.insert(src, track);
        if scheduled {
            self.playback.playing.store(true, Ordering::SeqCst);
        }
        self.notif_tx
            .try_push(PlayerNotification::Loaded { src: loaded_src })
            .ok();
        if let Some((direction, session_anchor_beats, track_anchor_beats)) = binding_event {
            self.notif_tx
                .try_push(PlayerNotification::BindingCommitted {
                    direction,
                    session_anchor_beats,
                    track_anchor_beats,
                })
                .ok();
        }
    }

    pub(super) fn poll_session_seek(&mut self) {
        let Some(pending) = self.session_seek else {
            return;
        };
        if self.playback.session_seek_cancelled(pending.attempt) {
            self.cancel_session_seek(pending);
            return;
        }
        let result = self
            .tracks
            .iter_mut()
            .find(|(_, track)| track.state().is_leading())
            .ok_or(())
            .and_then(|(_, track)| track.poll_session_seek(pending.revision).map_err(|_| ()));
        match result {
            Ok(true) => self.playback.prepare_session_seek(pending.attempt),
            Ok(false) => {}
            Err(()) => self.fail_session_seek(pending.attempt),
        }
    }
}
