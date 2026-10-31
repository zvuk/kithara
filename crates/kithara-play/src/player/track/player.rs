use kithara_command::{Batch, Outcome, Sender, Seq, Target, When};
use kithara_config::ConfigOwner;
use kithara_render::{
    CrossfadeSettings, LaneCommand,
    bridge::{DeckEvent, DeckPart, DeckProtocol, Fade, FadeDir, PlaybackFault, Returned, Slot},
};
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;
use tracing::warn;

use super::{
    super::outbox::{Bound, Outbox, Player, Settled, TrackReceipt},
    core::PlayerImpl,
    state::*,
};
use crate::PlayError;

impl<S> Player<S> for PlayerImpl<S> {
    type Command = TrackCommand<S>;
    type Snapshot = TrackSnapshot;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        if self.preparation_closed() {
            return None;
        }
        if matches!(
            self.status,
            TrackStatus::Loading | TrackStatus::Loaded | TrackStatus::Paused { .. }
        ) && self.ready != Some(self.segment)
        {
            return None;
        }
        match bound {
            Bound::AtOrAfter(frame) | Bound::AtOrBefore(frame) => Some(frame),
        }
    }

    fn apply(
        &mut self,
        command: TrackCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.observe(out);
        self.settle_lane();
        if !matches!(command, TrackCommand::Release)
            && let Some(failure) = self.preparation_failure()
        {
            return Err(PlayError::Lane(failure));
        }
        match command {
            TrackCommand::EffectProgram { lane, program } => {
                self.effect_program(lane, program, out)
            }
            TrackCommand::GainProgram(program) => self.gain_program(program, out),
            TrackCommand::Load { item, position } => self.load(item, position, out),
            command @ (TrackCommand::Play { .. }
            | TrackCommand::Pause { .. }
            | TrackCommand::Seek { .. }
            | TrackCommand::SetHostRate { .. }
            | TrackCommand::Jump { .. }
            | TrackCommand::Align { .. }
            | TrackCommand::Fade { .. }
            | TrackCommand::PlayAfter { .. }
            | TrackCommand::Supersede) => self.apply_transport(command, out),
            TrackCommand::Configure(change, at) => self.configure(change, at, out),
            TrackCommand::ConfigureEffect {
                lane,
                target,
                change,
                at,
            } => self.configure_effect(lane, target, change, at, out),
            TrackCommand::SetSpeed { speed, at } => self.set_speed(speed, at, out),
            TrackCommand::Release => self.release_track(out),
            TrackCommand::Seat { slot, at } => self.seat_at(slot, at, out),
        }
    }

    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled {
        match receipt {
            TrackReceipt::Loaded(receipt) => {
                self.observe(out);
                self.opened(receipt, out)
            }
            TrackReceipt::Deck {
                seq,
                outcome,
                batch,
            } if batch.basis.iter().any(|&(slot, _)| Some(slot) == self.slot)
                || batch.commands.iter().any(|part| matches!(part,
                    DeckPart::GainProgram { slot, binding, .. } if Some(*slot) == self.slot && binding.item() == self.item)) => {
                let attaching = self.attaching.is_some_and(|attach| attach.seq == Some(seq));
                let adopting = self.adopting.iter().any(|adopt| adopt.seq == seq);
                let playback = self.playback_commands.iter().position(|operation| {
                    operation.seq == Some(seq)
                        && operation
                            .basis
                            .iter()
                            .all(|basis| batch.basis.contains(basis))
                });
                if !attaching && !adopting && playback.is_none() {
                    self.settle_returned(batch, out);
                    return Settled::Pending;
                }
                let mut transport_current = self
                    .attaching
                    .filter(|attach| attach.seq == Some(seq))
                    .is_none_or(|attach| !attach.obsolete);
                if let Some(index) = playback {
                    let operation = self.playback_commands.remove(index);
                    transport_current &= !operation.obsolete;
                    self.settle_playback(&operation, outcome, batch, seq);
                }
                self.observe(out);
                transport_current &= !self.preparation_closed();
                self.applied(seq, outcome, batch, transport_current, out)
            }
            TrackReceipt::Deck { .. } => Settled::Pending,
            TrackReceipt::Event(event) => {
                self.observe(out);
                self.settle_event(event, out)
            }
        }
    }

    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, S>) {
        self.observe(out);
        self.settle_lane();
        self.fail_preparation(now);
        self.adopt_newest_epoch(out);
        self.promote(out);
        if self.status == TrackStatus::Released {
            if let Err(error) = self.release_lane(out) {
                warn!(%error, "lane release waits for room");
            }
        } else if self.releasing && self.attaching.is_none() && self.playback_commands.is_empty() {
            if let Err(error) = self.release_track(out) {
                warn!(%error, "released replacement waits to detach");
            }
        } else if self.attached()
            && self.ready == Some(self.segment)
            && let Some(at) = self.play
        {
            match self.play(at, out) {
                Ok(Some(_)) => self.play = None,
                Ok(None) => {}
                Err(error) => warn!(%error, "ready start was rejected"),
            }
        }
    }

    fn snapshot(&self) -> TrackSnapshot {
        TrackSnapshot {
            output: self.output,
            effects: self.effects.snapshot(self.lane_id),
            item: self.item,
            slot: self.slot,
            status: self.status,
            speed: self.settings.config().speed(),
            position: self.position,
            duration: self.duration,
            abr: self.abr.clone(),
            metadata: self.metadata.clone(),
            mark: self.mark,
            engine_latency: self.engine_latency,
            ring_depth: self.ring_depth,
            lane_room: self.lane.as_ref().map_or(0, Sender::available),
            pending_lane: self
                .lane_commands
                .iter()
                .any(|operation| operation.changes_time() && operation.applied().is_none())
                || self
                    .loading
                    .as_ref()
                    .is_some_and(|loading| loading.speed.is_some()),
            attached: self.attached(),
            declick: self.declick,
        }
    }
}

impl<S> PlayerImpl<S> {
    fn apply_transport(
        &mut self,
        command: TrackCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        match command {
            TrackCommand::Play { at } => self.play(at, out),
            TrackCommand::Pause { at } => self.pause(at, out),
            TrackCommand::Seek { to } => {
                if self.status == TrackStatus::Idle {
                    self.position = to;
                    return Ok(None);
                }
                let to = self.raw_position(to)?;
                self.segment(to, None, out)
            }
            TrackCommand::SetHostRate { rate } => self.segment(self.position, Some(rate), out),
            TrackCommand::Jump { to, at } => {
                let to = self.raw_position(to)?;
                let when = self.jump_when(at, out)?;
                self.send_lane(LaneCommand::Jump { to }, when).map(Some)
            }
            TrackCommand::Align { to, speed, at } => self.align(to, speed, at, out),
            TrackCommand::Fade { at, settings, dir } => self.fade(at, settings, dir, out),
            TrackCommand::PlayAfter { track } => self.play_after(track, out),
            TrackCommand::Supersede => self.supersede(out),
            _ => unreachable!("transport dispatch accepts only transport commands"),
        }
    }

    fn fade(
        &mut self,
        at: When<SessionFrame>,
        settings: CrossfadeSettings,
        dir: FadeDir,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let slot = self.seat()?;
        let staged = dir == FadeDir::In
            && out.is_grouped()
            && self.attaching.is_some_and(|attach| {
                attach.seq.is_none() && attach.replacement && attach.at == at
            });
        if !staged {
            self.check_when(at, out)?;
        }
        let basis = out.deck_basis(slot, at);
        let pending_stop = dir == FadeDir::In
            && self
                .pending_playback_action(slot, basis, PlaybackQuery::Admission)
                .is_some_and(|(_, action)| action == PlaybackAction::Stop);
        let part = match (self.status, dir) {
            (TrackStatus::Playing { .. }, _) if !pending_stop => DeckPart::Fade {
                slot,
                settings,
                dir,
            },
            (_, FadeDir::In) => DeckPart::Start {
                slot,
                fade: Fade::Crossfade(settings),
            },
            (_, FadeDir::Out) => return Ok(None),
        };
        self.send_playback(at, vec![part], out)
    }

    fn play_after(
        &mut self,
        track: Slot,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if Some(track) == self.slot {
            return self.repeat_after(out);
        }
        self.send_playback(
            When::Deferred,
            vec![DeckPart::Chain {
                from: track,
                to: self.seat()?,
            }],
            out,
        )
    }

    fn repeat_after(&mut self, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        let slot = self.seat()?;
        if out.is_grouped() {
            return Err(PlayError::Internal(
                "a repeat adoption cannot join a timed group".into(),
            ));
        }
        if self.lane.as_ref().is_none_or(|lane| lane.available() == 0) {
            return Err(PlayError::Full("lane"));
        }
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let parked_position = match self.resume.or(self.mark) {
            Some(mark) if mark.lane.segment == self.segment => mark.position,
            _ => self.raw_position(self.position)?,
        };
        let parked = (self.segment, parked_position);
        let segment = self.issued_segment.next();
        self.send_lane(
            LaneCommand::Segment {
                id: segment,
                from: Position::ZERO,
                speed: SpeedCurve::Constant(self.settings.projected().speed()),
            },
            When::Deferred,
        )?;
        self.issued_segment = segment;
        let seq = out.deferred(vec![DeckPart::Adopt { slot, segment }])?;
        self.segment = segment;
        self.segment_origin = Some(Position::ZERO);
        self.playback_commands.clear();
        self.segment_speed = self.settings.projected().speed();
        self.ready = None;
        self.repeat = true;
        self.adopting.push(Adoption {
            seq,
            caller: seq,
            segment,
            parked: Some(parked),
            cancelled: false,
        });
        Ok(Some(seq))
    }

    fn supersede(&mut self, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        if self
            .adopting
            .iter()
            .any(|adoption| adoption.parked.is_some())
            && self.lane.as_ref().is_none_or(|lane| lane.available() == 0)
        {
            return Err(PlayError::Full("lane"));
        }
        out.supersede(self.seat()?)?;
        for adoption in &mut self.adopting {
            if adoption.parked.is_some() {
                adoption.cancelled = true;
            }
        }
        self.newest_epoch_pending = None;
        self.repeat = false;
        Ok(None)
    }

    fn settle_returned(&mut self, batch: &Batch<DeckProtocol>, out: &mut Outbox<'_, S>) {
        if self.attached()
            && self.attaching.is_none()
            && self.status != TrackStatus::Released
            && batch.commands.iter().any(|part| {
                matches!(part, DeckPart::Returned(Returned::Pcm { slot, .. })
                if Some(*slot) == self.slot)
            })
        {
            if !self.preparation_closed() {
                self.status = TrackStatus::Released;
            }
            self.attach_at = None;
            self.releasing = false;
            self.mark = None;
            self.play = None;
            self.ready = None;
            self.playback_commands.clear();
            self.adopting.clear();
            self.newest_epoch_pending = None;
            if let Err(error) = self.release_lane(out) {
                warn!(%error, "lane release waits for dispatcher room");
            }
        }
    }

    fn settle_playback(
        &mut self,
        operation: &PlaybackOperation,
        outcome: &Outcome<DeckProtocol>,
        batch: &Batch<DeckProtocol>,
        seq: Seq,
    ) {
        if matches!(outcome, Outcome::Applied { .. })
            && let Some(slot) = self.slot
            && let Some((_, basis)) = operation.basis.iter().find(|(named, _)| *named == slot)
        {
            if operation.action.is_some() {
                self.obsolete_playback_ancestors(slot, *basis);
            } else if batch.commands.iter().any(|part| {
                matches!(part,
            DeckPart::Fade { slot: named, .. } if *named == slot)
            }) && let Some((ancestor, _)) =
                self.pending_playback_action(slot, *basis, PlaybackQuery::Receipt)
                && let Some(ancestor) = self.playback_commands.get_mut(ancestor)
            {
                ancestor.transparent = ancestor.transparent.max(Some(seq));
            }
        }
    }

    fn settle_event(&mut self, event: DeckEvent, out: &mut Outbox<'_, S>) -> Settled {
        if !self.attached() || matches!(self.status, TrackStatus::Failed { .. }) {
            return Settled::Pending;
        }
        match event {
            DeckEvent::Failed { slot, at, fault } if Some(slot) == self.slot => {
                if let TrackStatus::Playing { since } = self.status
                    && at.frames_since(since).is_some()
                {
                    self.status = TrackStatus::Failed { at, fault };
                }
            }
            DeckEvent::Ended { slot, at } if Some(slot) == self.slot && !self.repeat => {
                self.status = TrackStatus::Ended { at }
            }
            DeckEvent::Faded { slot, at }
                if Some(slot) == self.slot && !matches!(self.status, TrackStatus::Ended { .. }) =>
            {
                self.status = TrackStatus::Faded { at }
            }
            DeckEvent::UnderrunStarted { slot, item, .. }
            | DeckEvent::UnderrunEnded { slot, item, .. }
                if Some(slot) == self.slot && item == self.item && self.loading.is_none() =>
            {
                self.mark = out
                    .pass()
                    .and_then(|pass| pass.deck.slots.get(slot.index()))
                    .and_then(|slot| slot.mark);
            }
            _ => {}
        }
        self.promote(out);
        Settled::Pending
    }

    fn fail_preparation(&mut self, now: SessionFrame) {
        if self.status != TrackStatus::Released
            && let Some(failure) = self.preparation_failure()
        {
            if !matches!(self.status, TrackStatus::Failed { .. }) {
                self.status = TrackStatus::Failed {
                    at: now,
                    fault: PlaybackFault::Source(failure),
                };
            }
            self.ready = None;
            self.play = None;
            self.newest_epoch_pending = None;
            self.repeat = false;
            if let Some(attaching) = self.attaching.as_mut() {
                attaching.play = None;
                attaching.obsolete = true;
            }
        }
    }
}
