use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_play::{
    Bound, Outbox, OutputSnapshot, PlayError, Player, TrackCommand, TrackFactory,
    TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{
    Queue, Transition,
    command::play_error,
    slots::{LoadState, Role},
    types::Target,
};
use crate::{AdvanceReason, QueueError, QueueEvent, RepeatMode, TrackStatus};

#[derive(Clone, Copy)]
pub(in crate::queue) struct TransitionRequest {
    pub(in crate::queue) id: TrackId,
    pub(in crate::queue) transition: Transition,
    pub(in crate::queue) reason: AdvanceReason,
    pub(in crate::queue) auto: bool,
    pub(in crate::queue) playing: bool,
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(in crate::queue) fn auto_bound(
        &self,
        settings: kithara_play::CrossfadeSettings,
    ) -> Result<Option<Bound>, PlayError> {
        let Some(end) = self.current_end() else {
            return Ok(None);
        };
        let duration = if self.config.settings.gapless() {
            0.0
        } else {
            settings.duration
        };
        Ok(Some(Bound::AtOrBefore(end - self.fade_frames(duration)?)))
    }

    pub(in crate::queue) fn next_target(
        &mut self,
        transition: Transition,
        reason: AdvanceReason,
        auto: bool,
        playing: bool,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let ids = self.track_ids();
        let wrap = self.navigation.repeat_mode() == RepeatMode::All;
        self.navigation
            .next(&ids, auto, wrap)
            .map_or(Ok(None), |id| {
                self.request_transition(
                    TransitionRequest {
                        id,
                        transition,
                        reason,
                        auto,
                        playing,
                    },
                    output,
                    out,
                )
            })
    }

    pub(in crate::queue) fn request_transition(
        &mut self,
        request: TransitionRequest,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let TransitionRequest {
            id,
            transition,
            reason,
            auto,
            playing,
        } = request;
        let settings = transition
            .settings(self.config.settings.crossfade())
            .validate()
            .map_err(PlayError::from)?;
        if !auto && self.current == Some(id) {
            return self
                .transport(TrackCommand::Play { at: When::Next }, out)
                .map_err(Into::into);
        }
        if !auto
            && self
                .target
                .is_some_and(|target| target.to == id && !target.auto)
        {
            let target = self.target.as_mut().ok_or(PlayError::NotReady)?;
            target.playing = playing;
            target.settings = settings;
            target.transition = transition;
            target.reason = reason;
            if reason != AdvanceReason::InitialLoad {
                self.navigation.select(id, &self.track_ids());
            }
            return self.transition_loaded(out).map_err(Into::into);
        }
        let bound = if auto {
            self.auto_bound(settings)?.ok_or(PlayError::Untimed)?
        } else {
            Bound::AtOrAfter(self.earliest()?)
        };
        self.cancel_target(out)?;
        self.target = Some(Target {
            playing,
            to: id,
            bound,
            settings,
            transition,
            reason,
            auto,
            stale: None,
            retry: None,
            repeat: None,
            chained: false,
        });
        if !auto && reason != AdvanceReason::InitialLoad {
            self.navigation.select(id, &self.track_ids());
        }
        match self.load_track(id, Role::Incoming { batch: None }, output, out) {
            Ok(load) => match self.transition_loaded(out) {
                Ok(sent) => Ok(sent.or(load)),
                Err(error) => {
                    self.cancel_target(out)?;
                    Err(error.into())
                }
            },
            Err(error) => {
                self.target = None;
                self.tracks.fail(id, &error);
                self.announce(QueueEvent::TrackLoadFailed {
                    id,
                    reason: error.to_string(),
                    auto_skipped: false,
                });
                Err(error)
            }
        }
    }

    /// Step 2: an attached or prepared replacement can enter; an open in flight waits.
    pub(in crate::queue) fn transition_loaded(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let Some(target) = self
            .target
            .filter(|target| target.stale.is_none() && target.repeat.is_none())
        else {
            return Ok(None);
        };
        let Some(index) = self.incoming_index(target.to) else {
            return Ok(None);
        };
        let Some(active) = self.active.get(index) else {
            return Ok(None);
        };
        if !matches!(active.role, Role::Incoming { batch: None })
            || (matches!(active.load, Some(LoadState::Opening(_)))
                && !self.active.is_replacement(index))
        {
            return Ok(None);
        }
        if !matches!(
            active.track.snapshot().as_ref().status,
            PlayingStatus::Loaded | PlayingStatus::Paused { .. }
        ) {
            return Ok(None);
        }
        if let Some(load) = active.load {
            if !self.finish_load(index)? {
                self.cancel_target(out).map_err(play_error)?;
                return Ok(None);
            }
            if let Some(target) = &mut self.target {
                target.retry = Some(load.seq());
            }
        }
        if !target.playing {
            let sent = self
                .active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .apply(TrackCommand::Pause { at: When::Next }, out)?;
            if sent.is_none() {
                self.enter_target(index, self.earliest()?)?;
            }
            return Ok(sent);
        }
        let bound = if target.auto {
            let Some(bound) = self.auto_bound(target.settings)? else {
                return Ok(None);
            };
            self.target.as_mut().ok_or(PlayError::NotReady)?.bound = bound;
            bound
        } else {
            target.bound
        };
        let Some(frame) = self.transition_entry(index, bound)? else {
            return Ok(None);
        };
        let sent = self.send_transition(index, frame, out)?;
        if sent.is_some()
            && let Some(target) = &mut self.target
        {
            target.retry = None;
        }
        Ok(sent)
    }

    pub(in crate::queue) fn accept_load(&mut self, index: usize, seq: Seq) {
        let Some(active) = self.active.get_mut(index) else {
            return;
        };
        let id = active.item;
        let metadata = active.track.snapshot().as_ref().metadata.clone();
        active.load = Some(LoadState::Attaching(seq));
        if active.role != Role::Leaving {
            self.announce_loaded(id, &metadata);
        }
    }

    pub(in crate::queue) fn announce_loaded(&mut self, id: TrackId, metadata: &TrackMetadata) {
        if !self.tracks.loaded(id, metadata) {
            return;
        }
        if let Some(position) = self
            .tracks
            .records()
            .iter()
            .position(|record| record.id == id)
        {
            self.announce(QueueEvent::NextTrackReady {
                id,
                index: position,
            });
        }
    }

    pub(in crate::queue) fn finish_load(&mut self, index: usize) -> Result<bool, PlayError> {
        let active = self.active.get_mut(index).ok_or(PlayError::NoActiveSlot)?;
        active.load = None;
        Ok(self
            .tracks
            .records()
            .iter()
            .any(|record| record.id == active.item && record.status != TrackStatus::Cancelled))
    }

    /// Step 3: a deadline entry may not precede what this pass can deliver.
    pub(in crate::queue) fn transition_entry(
        &self,
        index: usize,
        bound: Bound,
    ) -> Result<Option<SessionFrame>, PlayError> {
        let earliest = self.earliest()?;
        let active = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let Some(frame) = active.track.entry(bound) else {
            return Ok(None);
        };
        Ok(if frame < earliest {
            active.track.entry(Bound::AtOrAfter(earliest))
        } else {
            Some(frame)
        })
    }
}
