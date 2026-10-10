use kithara_bufpool::HasPool;
use kithara_command::{Rejection, Seq, When};
use kithara_play::{
    Bound, FadeDir, Outbox, PlayError, Player, Settled, Track, TrackCommand, TrackFactory,
    TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{Queue, command::play_error, slots::Role};
use crate::{AdvanceReason, QueueError, QueueEvent, TrackStatus};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    /// Step 4: both envelopes, or the gapless chain, share one deck batch.
    pub(in crate::queue) fn send_transition(
        &mut self,
        incoming: usize,
        frame: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let target = self.target.ok_or(PlayError::NotReady)?;
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let slot = self
            .active
            .get(incoming)
            .ok_or(PlayError::NoActiveSlot)?
            .slot;
        let replacement = self.active.is_replacement(incoming);
        let current = self.active_current_index();
        let outgoing = current.filter(|index| {
            self.active
                .get(*index)
                .is_some_and(|current| current.slot != slot)
        });
        let predecessor = current
            .and_then(|index| self.active.get(index))
            .filter(|active| {
                matches!(
                    active.track.snapshot().as_ref().status,
                    PlayingStatus::Playing { .. }
                )
            })
            .map(|active| active.slot);
        let chain =
            target.auto && self.config.settings.gapless() && predecessor.is_some() && !replacement;
        let at = When::At(frame);
        let sent = if chain {
            Some(out.chain(predecessor.ok_or(PlayError::NoActiveSlot)?, slot)?)
        } else {
            let result = out.together_owned(at, |out| {
                let active = self
                    .active
                    .get_mut(incoming)
                    .ok_or(PlayError::NoActiveSlot)?;
                if replacement {
                    active.track.apply(TrackCommand::Seat { slot, at }, out)?;
                }
                active.track.apply(
                    TrackCommand::Fade {
                        at,
                        settings: target.settings,
                        dir: FadeDir::In,
                    },
                    out,
                )?;
                if let Some(index) = outgoing {
                    self.active
                        .get_mut(index)
                        .ok_or(PlayError::NoActiveSlot)?
                        .track
                        .apply(
                            TrackCommand::Fade {
                                at,
                                settings: target.settings,
                                dir: FadeDir::Out,
                            },
                            out,
                        )?;
                }
                Ok(())
            });
            let track = &mut self
                .active
                .get_mut(incoming)
                .ok_or(PlayError::NoActiveSlot)?
                .track;
            match result {
                Ok(((), Some(seq))) => {
                    track.finish_group(Ok(seq));
                    Some(seq)
                }
                Ok(((), None)) => None,
                Err((error, mut parts)) => {
                    track.finish_group(Err(&mut parts));
                    return Err(error);
                }
            }
        };
        if let Some(batch) = sent {
            self.active
                .get_mut(incoming)
                .ok_or(PlayError::NoActiveSlot)?
                .role = Role::Incoming { batch: Some(batch) };
            if let Some(target) = &mut self.target {
                target.chained = chain;
            }
        }
        Ok(sent)
    }

    /// Step 5: only the batch that starts the target changes the sounding item.
    pub(in crate::queue) fn transition_applied(
        &mut self,
        seq: Seq,
        at: SessionFrame,
    ) -> Result<(), PlayError> {
        let Some(target) = self.target else {
            return Ok(());
        };
        let Some(index) = self.incoming_index(target.to) else {
            return Ok(());
        };
        if !self
            .active
            .get(index)
            .is_some_and(|active| active.role == (Role::Incoming { batch: Some(seq) }))
        {
            return Ok(());
        }
        self.enter_target(index, at)
    }

    pub(in crate::queue) fn enter_target(
        &mut self,
        index: usize,
        at: SessionFrame,
    ) -> Result<(), PlayError> {
        let target = self.target.ok_or(PlayError::NotReady)?;
        if let Some(current) = self.active_current_index() {
            let slot = self
                .active
                .get(current)
                .ok_or(PlayError::NoActiveSlot)?
                .slot;
            self.active
                .get_mut(current)
                .ok_or(PlayError::NoActiveSlot)?
                .role = Role::Outgoing;
            if !target.chained {
                self.active
                    .fade_out(slot, at + self.fade_frames(target.settings.duration)?);
            }
        }
        self.active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .role = Role::Current;
        self.active.activate_replacement(index);
        self.current = Some(target.to);
        if target.auto || target.reason == AdvanceReason::InitialLoad {
            self.navigation.select(target.to, &self.track_ids());
        }
        self.target = None;
        self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
        self.announce(QueueEvent::CurrentTrackAdvance {
            id: self.current,
            reason: target.reason,
        });
        if target.playing && target.settings.duration > 0.0 && !target.chained {
            self.announce(QueueEvent::CrossfadeStarted {
                settings: target.settings,
            });
        }
        Ok(())
    }

    pub(in crate::queue) fn transition_settled(
        &mut self,
        index: usize,
        settled: &Settled,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let (seq, applied) = match settled {
            Settled::Pending => return Ok(()),
            Settled::Applied { seq, .. } => (*seq, true),
            Settled::Rejected { seq, .. } => (*seq, false),
        };
        let Some(active) = self.active.get(index) else {
            return Ok(());
        };
        if self.target.is_some_and(|target| target.repeat == Some(seq)) {
            match settled {
                Settled::Applied { .. } => {
                    self.target = None;
                    self.announce(QueueEvent::CurrentTrackAdvance {
                        id: self.current,
                        reason: AdvanceReason::NaturalEof,
                    });
                }
                Settled::Rejected {
                    reason: Rejection::Late | Rejection::Stale,
                    ..
                } => {
                    if let Some(target) = &mut self.target {
                        target.repeat = None;
                        target.retry = Some(seq);
                    }
                }
                _ => self.cancel_target(out).map_err(play_error)?,
            }
            return Ok(());
        }
        if active.load.is_some_and(|load| load.seq() == seq) {
            let id = active.item;
            let wanted = self
                .target
                .is_some_and(|target| self.incoming_index(target.to) == Some(index));
            if applied && self.finish_load(index)? {
                self.transition_loaded(out)?;
            } else if !applied {
                self.active
                    .get_mut(index)
                    .ok_or(PlayError::NoActiveSlot)?
                    .load = None;
                if let Settled::Rejected { reason, .. } = settled
                    && self.tracks.records().iter().any(|record| {
                        record.id == id
                            && matches!(
                                record.status,
                                TrackStatus::Loading | TrackStatus::Slow | TrackStatus::Loaded
                            )
                    })
                {
                    let error = QueueError::Play(super::super::hosted::refusal(reason));
                    self.tracks.fail(id, &error);
                    self.announce(QueueEvent::TrackLoadFailed {
                        id,
                        reason: error.to_string(),
                        auto_skipped: false,
                    });
                }
                self.release_track(index, out)?;
                if wanted {
                    self.target = None;
                }
            } else if wanted {
                self.cancel_target(out).map_err(play_error)?;
            }
            return Ok(());
        }
        if self.target.is_some_and(|target| target.stale == Some(seq))
            && matches!(active.role, Role::Incoming { .. })
        {
            match settled {
                Settled::Rejected {
                    reason: Rejection::Stale,
                    ..
                } => {
                    if self.target.is_some_and(|target| !target.playing) {
                        self.active
                            .get_mut(index)
                            .ok_or(PlayError::NoActiveSlot)?
                            .role = Role::Incoming { batch: None };
                        if matches!(
                            self.active
                                .get(index)
                                .ok_or(PlayError::NoActiveSlot)?
                                .track
                                .snapshot()
                                .as_ref()
                                .status,
                            PlayingStatus::Paused { .. }
                        ) {
                            self.target.as_mut().ok_or(PlayError::NotReady)?.stale = None;
                        }
                        return Ok(());
                    }
                    let target = self.target.as_mut().ok_or(PlayError::NotReady)?;
                    target.stale = None;
                    target.settings = target.transition.settings(self.config.settings.crossfade());
                    self.active
                        .get_mut(index)
                        .ok_or(PlayError::NoActiveSlot)?
                        .role = Role::Incoming { batch: None };
                    if let Some(target) = &mut self.target {
                        target.retry = Some(seq);
                    }
                }
                Settled::Applied { at, .. } => self.transition_applied(seq, *at)?,
                _ => {
                    self.cancel_target(out).map_err(play_error)?;
                }
            }
            return Ok(());
        }
        if applied
            && self
                .target
                .is_some_and(|target| !target.playing && target.stale.is_some())
            && active.role == (Role::Incoming { batch: None })
            && matches!(
                active.track.snapshot().as_ref().status,
                PlayingStatus::Paused { .. }
            )
        {
            self.target.as_mut().ok_or(PlayError::NotReady)?.stale = None;
            return self.enter_target(index, self.earliest()?);
        }
        match settled {
            Settled::Applied { at, .. } => self.transition_applied(seq, *at),
            Settled::Rejected { reason, .. }
                if self
                    .active
                    .get(index)
                    .is_some_and(|active| active.role == (Role::Incoming { batch: Some(seq) })) =>
            {
                if matches!(reason, Rejection::Late | Rejection::Stale) {
                    if let Some(active) = self.active.get_mut(index) {
                        active.role = Role::Incoming { batch: None };
                    }
                    let earliest = self.earliest()?;
                    if let Some(target) = &mut self.target {
                        target.retry = Some(seq);
                        target.bound = Bound::AtOrAfter(earliest);
                    }
                    Ok(())
                } else {
                    self.cancel_target(out).map_err(play_error)
                }
            }
            _ => Ok(()),
        }
    }
}
