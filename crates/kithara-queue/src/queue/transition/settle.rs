use kithara_bufpool::HasPool;
use kithara_command::{Batch, Outcome, Rejection, Seq, When};
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{
    Bound, DeckPart, DeckProtocol, FadeDir, Outbox, PlayError, Player, Settled, TrackCommand,
    TrackFactory, TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{
    Queue,
    command::play_error,
    slots::Role,
    types::{Continuation, SeekPosition, Target},
};
use crate::{AdvanceReason, QueueEvent, TrackStatus};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory,
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
        let withdrawn = self
            .target
            .is_some_and(|target| target.to == id && !target.auto && target.stale == Some(seq));
        if self.target.is_none_or(|target| target.to != id) || withdrawn {
            let duration = self.replacement_duration(id);
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
            let source_changed = self.commit_source(id, duration);
            if !source_changed {
                self.navigation
                    .commit(self.current, id, pending.reason, &self.track_ids());
            }
            self.current = Some(id);
            if withdrawn {
                self.target.as_mut().ok_or(PlayError::NotReady)?.stale = None;
            }
            self.tracks.set_status(id, TrackStatus::Consumed);
            if !source_changed {
                self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
                self.announce(QueueEvent::CurrentTrackAdvance {
                    id: self.current,
                    reason: pending.reason,
                });
            }
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
        out: &mut Outbox<'_>,
    ) -> Result<(), PlayError> {
        let seq = match settled {
            Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => *seq,
            Settled::Pending => return Ok(()),
        };
        let Some(pending) = self.pending_handover.filter(|pending| pending.seq == seq) else {
            return Ok(());
        };
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
            if self.tracks.restore_source(pending.item)
                && self.target.is_some_and(|target| target.to == pending.item)
            {
                self.target = None;
            }
            return Ok(());
        };
        if let Settled::Applied { at, .. } = settled {
            self.commit_handover(seq, index, *at)?;
            return Ok(());
        }
        self.pending_handover = None;
        let item = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.item;
        if self.target.is_none_or(|target| target.to != item) {
            self.tracks.restore_source(item);
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
        out: &mut Outbox<'_>,
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
            let id = self.active.get(index).ok_or(PlayError::NoActiveSlot)?.item;
            let changing = self.tracks.is_changing_source(id);
            self.cancel_target(out).map_err(play_error)?;
            if changing {
                tracing::debug!(?id, ?reason, "source continuation rejected by the deck");
                self.tracks.fail_source(id);
            }
            Ok(())
        }
    }

    pub(in crate::queue) fn enter_target(
        &mut self,
        index: usize,
        at: SessionFrame,
        reason: AdvanceReason,
    ) -> Result<(), PlayError> {
        let target = self.target.ok_or(PlayError::NotReady)?;
        let duration = self.replacement_duration(target.to);
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
        let source_changed = self.commit_source(target.to, duration);
        if !source_changed {
            self.navigation
                .commit(self.current, target.to, reason, &self.track_ids());
        }
        self.current = Some(target.to);
        if crossfading && target.playing && target.settings.duration > 0.0 && !target.chained {
            self.announce(QueueEvent::CrossfadeStarted {
                settings: target.settings,
            });
        }
        self.target = None;
        if !source_changed {
            self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
            self.announce(QueueEvent::CurrentTrackAdvance {
                id: self.current,
                reason,
            });
        }
        self.tracks.set_status(target.to, TrackStatus::Consumed);
        Ok(())
    }

    /// Commits `id`'s staged source. The replacement opened at the seek
    /// target the outgoing receiver still holds pending, so that seek is done.
    fn commit_source(&mut self, id: TrackId, duration: Option<Duration>) -> bool {
        let changed = self.tracks.commit_source(id, duration);
        if changed
            && matches!(self.held_position, Some(SeekPosition::Pending { item, .. }) if item == id)
        {
            self.held_position = None;
        }
        changed
    }

    fn replacement_duration(&self, id: TrackId) -> Option<Duration> {
        if !self.tracks.is_changing_source(id) {
            return None;
        }
        self.active_current_index()
            .and_then(|index| self.active.get(index))
            .filter(|outgoing| outgoing.item == id)
            .and_then(|outgoing| outgoing.track.snapshot().as_ref().duration)
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory,
{
    pub(in crate::queue) fn transition_settled(
        &mut self,
        index: usize,
        settled: &Settled,
        out: &mut Outbox<'_>,
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
        if applied
            && self.current.is_none()
            && active.role == Role::Current
            && matches!(
                active.track.snapshot().as_ref().status,
                PlayingStatus::Loaded | PlayingStatus::Playing { .. }
            )
        {
            self.current = Some(active.item);
            self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
        }
        self.pause_settled(index, seq, applied, settled, out)
    }
    fn repeat_settled(
        &mut self,
        seq: Seq,
        settled: &Settled,
        out: &mut Outbox<'_>,
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

    fn continuation_rejected(
        &mut self,
        index: usize,
        reason: &Rejection<PlayError>,
        out: &mut Outbox<'_>,
    ) -> Result<(), PlayError> {
        if retries(reason, self.active.is_replacement(index)) {
            Ok(())
        } else {
            self.cancel_target(out).map_err(play_error)
        }
    }

    fn pause_settled(
        &mut self,
        index: usize,
        seq: Seq,
        applied: bool,
        settled: &Settled,
        out: &mut Outbox<'_>,
    ) -> Result<(), PlayError> {
        let active = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let Some(target) = self
            .target
            .filter(|target| active.role == Role::Incoming && target.to == active.item)
        else {
            return Ok(());
        };
        let continued = settled_continuation(target, seq);
        let seated = matches!(continued, Some(Continuation::Seat(_)));
        let paused = held_paused(&active.track.snapshot().as_ref().status, seated);
        if continued.is_some() {
            self.target.as_mut().ok_or(PlayError::NotReady)?.continued = None;
            if let Settled::Rejected { reason, .. } = settled {
                return self.continuation_rejected(index, reason, out);
            }
        }
        if !applied || !paused || !enters(target, continued) {
            return Ok(());
        }
        self.target.as_mut().ok_or(PlayError::NotReady)?.stale = None;
        self.enter_target(index, self.earliest()?, target.reason)?;
        if target.playing {
            self.transport(TrackCommand::Play { at: When::Next }, out)?;
        }
        Ok(())
    }
}

/// The continuation a pause receipt `seq` settles: a seat settles whatever
/// transport arrived since, a pause only while the target still wants it
/// paused.
fn settled_continuation(target: Target, seq: Seq) -> Option<Continuation> {
    match target.continued {
        Some(seat @ Continuation::Seat(owner)) if owner == seq => Some(seat),
        Some(pause @ Continuation::Pause(owner)) if owner == seq && !target.playing => Some(pause),
        _ => None,
    }
}

/// Whether an applied, held receipt enters the target: its own settled
/// continuation does, and a paused target recomputing after a stale receipt.
fn enters(target: Target, continued: Option<Continuation>) -> bool {
    continued.is_some() || !target.playing && target.stale.is_some()
}

/// Whether `status` holds the paused frame a paused target waits for; a
/// `seated` replacement lands loaded without a start.
fn held_paused(status: &PlayingStatus, seated: bool) -> bool {
    match status {
        PlayingStatus::Paused { .. } => true,
        PlayingStatus::Loaded => seated,
        _ => false,
    }
}

/// Whether a paused target outlives the rejection of its continuation; a
/// refused seat releases the replacement, so it cannot retry.
fn retries<R>(reason: &Rejection<R>, replacement: bool) -> bool {
    matches!(reason, Rejection::Late | Rejection::Stale) && !replacement
}
