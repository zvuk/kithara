use kithara_bufpool::HasPool;
use kithara_command::{Batch, Outcome, Rejection, Seq};
use kithara_play::{
    Bound, DeckPart, DeckProtocol, FadeDir, Outbox, PlayError, Player, Settled, TrackFactory,
    TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{Queue, command::play_error, slots::Role, types::Continuation};
use crate::{AdvanceReason, QueueEvent, TrackStatus};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(in crate::queue) fn abandon_removal_entry(&mut self) -> Result<(), PlayError> {
        if let Some(target) = self.target.take()
            && let Some(incoming) = self.incoming_index(target.to)
        {
            self.active
                .get_mut(incoming)
                .ok_or(PlayError::NoActiveSlot)?
                .role = Role::Preloaded;
        }
        Ok(())
    }

    /// Step 5: only the batch that starts the target changes the sounding item.
    pub(in crate::queue) fn transition_applied(
        &mut self,
        seq: Seq,
        index: usize,
        at: SessionFrame,
    ) -> Result<(), PlayError> {
        let Some(pending) = self.pending_handover.filter(|pending| {
            pending.seq == seq
                && self.active.get(index).is_some_and(|active| {
                    active.item == pending.item
                        && active.slot == pending.slot
                        && self.active.is_replacement(index) == pending.replacement
                })
        }) else {
            return Ok(());
        };
        self.pending_handover = None;
        let id = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.item;
        if self.target.is_none_or(|target| target.to != id) {
            if let Some(current) = self.active_current_index() {
                self.active
                    .get_mut(current)
                    .ok_or(PlayError::NoActiveSlot)?
                    .role = Role::Outgoing;
            }
            self.active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .role = Role::Current;
            self.active.activate_replacement(index);
            self.navigation
                .commit(self.current, id, pending.reason, &self.track_ids());
            self.current = Some(id);
            self.tracks.set_status(id, TrackStatus::Consumed);
            self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
            self.announce(QueueEvent::CurrentTrackAdvance {
                id: self.current,
                reason: pending.reason,
            });
            return Ok(());
        }
        self.enter_target(index, at, pending.reason)
    }

    pub(in crate::queue) fn settle_handover(
        &self,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
        batch: &Batch<DeckProtocol>,
    ) -> Option<(Option<usize>, Settled)> {
        let pending = self.pending_handover.filter(|pending| pending.seq == seq)?;
        if self
            .active
            .iter()
            .enumerate()
            .find(|(index, active)| {
                active.item == pending.item
                    && active.slot == pending.slot
                    && self.active.is_replacement(*index) == pending.replacement
            })
            .is_some_and(|(_, active)| !batch.basis.iter().any(|&(slot, _)| slot == active.slot))
        {
            return None;
        }
        let index = self.active.iter().enumerate().find_map(|(index, active)| {
            (active.item == pending.item
                && active.slot == pending.slot
                && self.active.is_replacement(index) == pending.replacement
                && batch.basis.iter().any(|&(slot, _)| slot == active.slot)
                && batch.commands.iter().any(|part| match part {
                    DeckPart::Start { slot, .. }
                    | DeckPart::Chain { to: slot, .. }
                    | DeckPart::Fade {
                        slot,
                        dir: FadeDir::In,
                        ..
                    } => *slot == active.slot,
                    _ => false,
                }))
            .then_some(index)
        });
        let settled = match outcome {
            Outcome::Applied { at, .. } => Settled::Applied { seq, at: *at },
            Outcome::Rejected(reason) => Settled::Rejected {
                seq,
                reason: match reason {
                    Rejection::Late => Rejection::Late,
                    Rejection::Stale => Rejection::Stale,
                    Rejection::Unanswered => Rejection::Unanswered,
                    Rejection::Refused(reason) => Rejection::Refused(PlayError::Deck(*reason)),
                },
            },
        };
        Some((index, settled))
    }

    pub(in crate::queue) fn handover_settled(
        &mut self,
        index: Option<usize>,
        settled: &Settled,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let seq = match settled {
            Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => *seq,
            Settled::Pending => return Ok(()),
        };
        if self
            .pending_handover
            .is_none_or(|pending| pending.seq != seq)
        {
            return Ok(());
        }
        if matches!(settled, Settled::Rejected { .. })
            && self.target.is_some_and(|target| {
                target.reason == AdvanceReason::RemovedCurrent
                    && matches!(target.continued,
                    Some(Continuation::Removal { handover, .. }) if handover == seq)
            })
        {
            self.abandon_removal_entry()?;
        }
        let Some(index) = index else {
            self.pending_handover = None;
            return Ok(());
        };
        if let Settled::Applied { at, .. } = settled {
            self.commit_handover(seq, index, *at)?;
            return Ok(());
        }
        self.pending_handover = None;
        let item = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.item;
        if self.target.is_none_or(|target| target.to != item) {
            if self.current != Some(item)
                && self
                    .active
                    .get(index)
                    .is_some_and(|active| active.role != Role::Leaving)
            {
                self.release_track(index, out)?;
            }
            return Ok(());
        }
        let Settled::Rejected { reason, .. } = settled else {
            return Ok(());
        };
        self.recover_handover(index, seq, reason, out)
    }

    fn commit_handover(
        &mut self,
        seq: Seq,
        index: usize,
        at: SessionFrame,
    ) -> Result<(), PlayError> {
        self.transition_applied(seq, index, at)?;
        if let Some(target) = self.target.filter(|target| !target.auto)
            && self.incoming_index(target.to).is_none()
            && let Some(requested) = self
                .active
                .position(|active| active.item == target.to && active.role == Role::Outgoing)
        {
            self.active
                .get_mut(requested)
                .ok_or(PlayError::NoActiveSlot)?
                .role = Role::Incoming;
        }
        Ok(())
    }

    fn recover_handover(
        &mut self,
        index: usize,
        seq: Seq,
        reason: &Rejection<PlayError>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        if matches!(reason, Rejection::Stale)
            && self.target.is_some_and(|target| target.stale == Some(seq))
        {
            if self.target.is_some_and(|target| !target.playing) {
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
            target.continued = Some(Continuation::Entry(seq));
            return Ok(());
        }
        if matches!(reason, Rejection::Late | Rejection::Stale) {
            let earliest = self.earliest()?;
            if let Some(target) = &mut self.target {
                target.continued = Some(Continuation::Entry(seq));
                target.bound = Some(Bound::AtOrAfter(earliest));
            }
            Ok(())
        } else {
            self.cancel_target(out).map_err(play_error)
        }
    }

    pub(in crate::queue) fn enter_target(
        &mut self,
        index: usize,
        at: SessionFrame,
        reason: AdvanceReason,
    ) -> Result<(), PlayError> {
        let target = self.target.ok_or(PlayError::NotReady)?;
        let outgoing = self.active_current_index();
        let incoming_slot = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.slot;
        let crossfading = outgoing
            .and_then(|current| self.active.get(current))
            .is_some_and(|active| active.slot != incoming_slot);
        if let Some(current) = outgoing {
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
        self.navigation
            .commit(self.current, target.to, reason, &self.track_ids());
        self.current = Some(target.to);
        if crossfading && target.playing && target.settings.duration > 0.0 && !target.chained {
            self.announce(QueueEvent::CrossfadeStarted {
                settings: target.settings,
            });
        }
        self.target = None;
        self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
        self.announce(QueueEvent::CurrentTrackAdvance {
            id: self.current,
            reason,
        });
        self.tracks.set_status(target.to, TrackStatus::Consumed);
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
            self.repeat_settled(seq, settled, out)?;
            return Ok(());
        }
        if active.load.is_some_and(|load| load.seq() == seq) {
            return self.load_settled(index, settled, out);
        }
        self.pause_settled(index, seq, applied, settled, out)
    }
    fn repeat_settled(
        &mut self,
        seq: Seq,
        settled: &Settled,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
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
                    target.continued = Some(Continuation::Entry(seq));
                }
            }
            _ => self.cancel_target(out).map_err(play_error)?,
        }
        Ok(())
    }

    fn pause_settled(
        &mut self,
        index: usize,
        seq: Seq,
        applied: bool,
        settled: &Settled,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let active = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let continued_pause = active.role == Role::Incoming
            && self.target.is_some_and(|target| {
                !target.playing
                    && target.to == active.item
                    && target.continued == Some(Continuation::Pause(seq))
            });
        let paused = matches!(
            active.track.snapshot().as_ref().status,
            PlayingStatus::Paused { .. }
        );
        if continued_pause {
            self.target.as_mut().ok_or(PlayError::NotReady)?.continued = None;
            if let Settled::Rejected { reason, .. } = settled {
                return if matches!(reason, Rejection::Late | Rejection::Stale) {
                    Ok(())
                } else {
                    self.cancel_target(out).map_err(play_error)
                };
            }
        }
        if applied
            && self.target.is_some_and(|target| {
                !target.playing
                    && target.to == active.item
                    && (target.stale.is_some() || continued_pause)
            })
            && active.role == Role::Incoming
            && paused
        {
            self.target.as_mut().ok_or(PlayError::NotReady)?.stale = None;
            let reason = self.target.ok_or(PlayError::NotReady)?.reason;
            return self.enter_target(index, self.earliest()?, reason);
        }
        Ok(())
    }
}
