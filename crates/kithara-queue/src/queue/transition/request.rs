use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_play::{
    Bound, CrossfadeSettings, Outbox, OutputSnapshot, PlayError, Player, Settled, TrackCommand,
    TrackFactory, TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{
    Queue, Transition,
    command::play_error,
    slots::{LoadState, Role},
    types::{Continuation, Target},
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
        settings: CrossfadeSettings,
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
        let ids = self.selectable_ids();
        let wrap = self.navigation.repeat_mode() == RepeatMode::All;
        let cursor = self.navigation_cursor();
        let Some(id) = self.navigation.next(
            &ids,
            cursor,
            auto && reason != AdvanceReason::TrackFailed,
            wrap,
        ) else {
            if auto {
                self.announce(QueueEvent::QueueEnded);
            }
            return Ok(None);
        };
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
            ..
        } = request;
        let settings = transition
            .settings(self.config.settings.crossfade())
            .validate()
            .map_err(PlayError::from)?;
        let failed = self
            .tracks
            .records()
            .iter()
            .any(|record| record.id == id && matches!(record.status, TrackStatus::Failed(_)));
        if !auto
            && !failed
            && self.current == Some(id)
            && self.pending_handover.is_none_or(|pending| {
                reason == AdvanceReason::UserSelect
                    && self.target.is_some_and(|target| {
                        target.auto
                            && target.chained
                            && target.reason == AdvanceReason::NaturalEof
                            && target.to == pending.item
                    })
            })
        {
            if self.target.is_some_and(|target| !target.auto) {
                self.cancel_target(out)?;
            }
            return self
                .transport(TrackCommand::Play { at: When::Next }, out)
                .map_err(Into::into);
        }
        if !auto
            && !failed
            && self
                .target
                .is_some_and(|target| target.to == id && !target.auto)
        {
            return self.retarget_request(request, settings, out);
        }
        let bound = if auto && reason == AdvanceReason::NaturalEof {
            self.auto_bound(settings)?
        } else {
            Some(Bound::AtOrAfter(self.earliest()?))
        };
        self.begin_transition(request, settings, bound, output, out)
    }

    fn retarget_request(
        &mut self,
        request: TransitionRequest,
        settings: CrossfadeSettings,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let TransitionRequest {
            id,
            transition,
            reason,
            playing,
            ..
        } = request;
        let target = self.target.as_mut().ok_or(PlayError::NotReady)?;
        target.set_playing(playing);
        target.settings = settings;
        target.transition = transition;
        target.reason = reason;
        let load = self
            .incoming_index(id)
            .and_then(|index| self.active.get(index))
            .and_then(|active| active.load)
            .map(LoadState::seq);
        self.transition_loaded(out)
            .map(|sent| sent.or(load))
            .map_err(Into::into)
    }

    fn begin_transition(
        &mut self,
        request: TransitionRequest,
        settings: CrossfadeSettings,
        bound: Option<Bound>,
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
        self.cancel_target(out)?;
        if !auto {
            for index in self.active.indices(|active| {
                active.item != id && active.role == Role::Preloaded && active.load.is_none()
            }) {
                let item = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.item;
                self.release_track(index, out)?;
                self.tracks.set_status(item, TrackStatus::Consumed);
            }
            self.reap_released();
        }
        self.target = Some(Target {
            playing,
            to: id,
            bound,
            settings,
            transition,
            reason,
            auto,
            stale: None,
            continued: None,
            repeat: None,
            chained: false,
        });
        match self.load_track(id, Role::Incoming, output, out) {
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
                Err(error)
            }
        }
    }

    pub(super) fn initial_attachment(&self, index: usize) -> Result<bool, PlayError> {
        let prepared = self.current.is_none()
            && self
                .active
                .iter()
                .all(|active| matches!(active.role, Role::Incoming | Role::Preloaded))
            && !self.active.is_replacement(index)
            && self.active.get(index).is_some_and(|active| {
                let snapshot = active.track.snapshot();
                matches!(active.load, Some(LoadState::Attaching(_)))
                    && snapshot.as_ref().status == PlayingStatus::Loading
                    && snapshot.as_ref().attached
            });
        if !prepared {
            return Ok(false);
        }
        let earliest = self.earliest()?;
        Ok(self.target.is_some_and(
            |target| matches!(target.bound, Some(Bound::AtOrAfter(frame)) if frame <= earliest),
        ))
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(in crate::queue) fn transition_loaded(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let Some(target) = self.target.filter(|target| {
            target.stale.is_none()
                && target.repeat.is_none()
                && !matches!(
                    target.continued,
                    Some(Continuation::Removal { applied: false, .. })
                )
        }) else {
            return Ok(None);
        };
        if self.pending_handover.is_some() {
            return Ok(None);
        }
        if let Some(index) = self.current_target_index(target) {
            return self.resume_target(index, target.playing, out);
        }
        let Some(index) = self.incoming_index(target.to) else {
            return Ok(None);
        };
        if !self.prepare_target(index, target, out)? {
            return Ok(None);
        }
        if !target.playing {
            return self.pause_target(index, target, out);
        }
        let Some(bound) = self.target_entry_bound(target)? else {
            return Ok(None);
        };
        let Some(frame) = self.transition_entry(index, bound)? else {
            return Ok(None);
        };
        let sent = self.send_transition(index, frame, out)?;
        if sent.is_some()
            && let Some(target) = &mut self.target
        {
            target.continued = None;
        }
        Ok(sent)
    }

    fn current_target_index(&self, target: Target) -> Option<usize> {
        if !target.auto
            && self.current == Some(target.to)
            && !self.tracks.records().iter().any(|record| {
                record.id == target.to && matches!(record.status, TrackStatus::Failed(_))
            })
            && self.incoming_index(target.to).is_none()
        {
            self.active_current_index()
        } else {
            None
        }
    }

    fn resume_target(
        &mut self,
        index: usize,
        playing: bool,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let command = if playing {
            TrackCommand::Play { at: When::Next }
        } else {
            TrackCommand::Pause { at: When::Next }
        };
        let sent = self
            .active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .apply(command, out)?;
        self.target = None;
        Ok(sent)
    }

    fn prepare_target(
        &mut self,
        index: usize,
        target: Target,
        out: &mut Outbox<'_, S>,
    ) -> Result<bool, PlayError> {
        let Some(active) = self.active.get(index) else {
            return Ok(false);
        };
        if !matches!(active.role, Role::Incoming)
            || (matches!(active.load, Some(LoadState::Opening(_)))
                && !self.active.is_replacement(index))
        {
            return Ok(false);
        }
        let initial_attachment = target.playing && self.initial_attachment(index)?;
        if !initial_attachment
            && !matches!(
                active.track.snapshot().as_ref().status,
                PlayingStatus::Loaded
                    | PlayingStatus::Paused { .. }
                    | PlayingStatus::Playing { .. }
                    | PlayingStatus::Faded { .. }
            )
        {
            return Ok(false);
        }
        if let Some(load) = active.load.filter(|_| !initial_attachment) {
            if !self.finish_load(index)? {
                self.cancel_target(out).map_err(play_error)?;
                return Ok(false);
            }
            if let Some(target) = &mut self.target {
                target.continued = Some(Continuation::Load(load.seq()));
            }
        }
        Ok(true)
    }

    fn pause_target(
        &mut self,
        index: usize,
        target: Target,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if self
            .target
            .is_some_and(|target| matches!(target.continued, Some(Continuation::Pause(_))))
        {
            return Ok(None);
        }
        let sent = self
            .active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .apply(TrackCommand::Pause { at: When::Next }, out)?;
        if let Some(seq) = sent {
            self.target.as_mut().ok_or(PlayError::NotReady)?.continued =
                Some(Continuation::Pause(seq));
        } else {
            self.enter_target(index, self.earliest()?, target.reason)?;
        }
        Ok(sent)
    }

    fn target_entry_bound(&mut self, target: Target) -> Result<Option<Bound>, PlayError> {
        let bound = if target.auto && target.reason == AdvanceReason::NaturalEof {
            let Some(bound) = self.auto_bound(target.settings)? else {
                return Ok(None);
            };
            self.target.as_mut().ok_or(PlayError::NotReady)?.bound = Some(bound);
            bound
        } else if target.reason == AdvanceReason::RemovedCurrent
            || matches!(
                target.continued,
                Some(Continuation::Removal { applied: true, .. })
            )
        {
            let bound = Bound::AtOrAfter(self.earliest()?);
            self.target.as_mut().ok_or(PlayError::NotReady)?.bound = Some(bound);
            bound
        } else {
            target.bound.ok_or(PlayError::Untimed)?
        };
        Ok(Some(bound))
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

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
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

    pub(super) fn load_settled(
        &mut self,
        index: usize,
        settled: &Settled,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let _ = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let wanted = self
            .target
            .is_some_and(|target| self.incoming_index(target.to) == Some(index));
        let applied = matches!(settled, Settled::Applied { .. });
        if applied && self.finish_load(index)? {
            self.transition_loaded(out)?;
        } else if !applied {
            self.reject_load(index, settled, wanted, out)?;
        } else if wanted {
            self.cancel_target(out).map_err(play_error)?;
        }
        Ok(())
    }
    fn reject_load(
        &mut self,
        index: usize,
        settled: &Settled,
        wanted: bool,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let active = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let id = active.item;
        let preloaded = active.role == Role::Preloaded;
        let leaving = active.role == Role::Leaving;
        self.active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .load = None;
        if let Settled::Rejected { reason, .. } = settled
            && !leaving
            && self.tracks.records().iter().any(|record| {
                record.id == id
                    && matches!(
                        record.status,
                        TrackStatus::Loading | TrackStatus::Slow | TrackStatus::Loaded
                    )
            })
        {
            if self
                .target
                .is_some_and(|target| target.to == id && target.auto)
                || preloaded
            {
                self.tracks.set_status(id, TrackStatus::Consumed);
            } else {
                let error = QueueError::Play(super::super::hosted::refusal(reason));
                self.tracks.fail(id, &error);
                self.announce(QueueEvent::TrackLoadFailed {
                    id,
                    reason: error.to_string(),
                    auto_skipped: false,
                });
            }
        }
        self.release_track(index, out)?;
        if wanted
            && (self.target.is_some_and(|target| target.auto)
                || !self.tracks.records().iter().any(|record| {
                    record.id == id && matches!(record.status, TrackStatus::Failed(_))
                }))
        {
            self.target = None;
        }
        Ok(())
    }
}
