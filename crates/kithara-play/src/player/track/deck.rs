//! Playback commands, clock and gain programs sent to a seated RT track.

use kithara_command::{Seq, Target, When};
use kithara_config::LiveConfig;
use kithara_render::{
    LaneCommand,
    bridge::{DeckPart, Fade, Slot, SlotGainProgram, SlotMark, SlotState, TrackClock},
};
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;

use super::{core::PlayerImpl, geometry::*, state::*};
use crate::{Outbox, PlayError, TrackSettings, TrackSettingsChange};

impl TryFrom<(&DeckPart, Slot)> for PlaybackAction {
    type Error = ();

    fn try_from((part, slot): (&DeckPart, Slot)) -> Result<Self, Self::Error> {
        match part {
            DeckPart::Start { slot: started, .. } if *started == slot => Ok(Self::Start),
            DeckPart::Chain { to, .. } if *to == slot => Ok(Self::Start),
            DeckPart::Stop { slot: stopped, .. } if *stopped == slot => Ok(Self::Stop),
            DeckPart::Detach { slot: detached } if *detached == slot => Ok(Self::Detach),
            _ => Err(()),
        }
    }
}

impl PlayerImpl {
    /// Seat parts carry the clock a fresh RT track lacks.
    pub(super) fn seated_with_clock(
        &mut self,
        slot: Slot,
        mut parts: Vec<DeckPart>,
    ) -> Vec<DeckPart> {
        if self.clock != TrackClock::Media {
            parts.push(DeckPart::Clock {
                slot,
                clock: self.clock,
            });
        }
        self.seated_clock = self.clock;
        parts
    }

    /// Sends a changed clock to the RT track the deck has already seated; a
    /// seat still in flight carries it itself.
    pub(super) fn sync_clock(&mut self, out: &mut Outbox<'_>) -> Result<Option<Seq>, PlayError> {
        let Some(slot) = self.seat.slot() else {
            return Ok(None);
        };
        if self.clock == self.seated_clock || self.seat.attached_at().is_none() {
            return Ok(None);
        }
        let sent = out.deck(
            When::Next,
            vec![DeckPart::Clock {
                slot,
                clock: self.clock,
            }],
        )?;
        self.seated_clock = self.clock;
        Ok(sent)
    }

    pub(super) fn gain_program(
        &mut self,
        program: SlotGainProgram,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        if self.seat.releasing()
            || self.preparation_closed()
            || !self.attached()
            || !self.segment.ready
            || matches!(
                self.status,
                TrackStatus::Idle | TrackStatus::Loading | TrackStatus::Ended { .. }
            )
        {
            return Err(PlayError::NotReady);
        }
        let at = When::At(program.start());
        self.check_when(at, out)?;
        let slot = self.seat()?;
        let observed = out
            .pass()
            .and_then(|pass| pass.deck.slots.get(slot.index()))
            .ok_or(PlayError::NotReady)?;
        let binding = observed
            .binding
            .filter(|binding| binding.item() == self.item && binding.segment() == self.segment.id)
            .filter(|_| observed.state != SlotState::Ended)
            .ok_or(PlayError::NotReady)?;
        self.send_playback(
            at,
            vec![DeckPart::GainProgram {
                slot,
                binding,
                authority: observed.output.authority(),
                program,
            }],
            out,
        )
    }

    pub(super) fn release_track(&mut self, out: &mut Outbox<'_>) -> Result<Option<Seq>, PlayError> {
        self.withdraw_play();
        if let Some(loading) = self.loading.as_mut() {
            loading.speed = None;
        }
        if let Some(seq) = self.playback_commands.iter().rev().find_map(|operation| {
            operation
                .seq
                .filter(|_| operation.action == Some(PlaybackAction::Detach))
        }) {
            return Ok(Some(seq));
        }
        self.playback_commands.clear();
        if self.status == TrackStatus::Released {
            self.release_lane(out)?;
            return Ok(None);
        }
        if self
            .seat
            .attaching()
            .is_some_and(|attach| attach.replacement && attach.seq.is_some())
        {
            if !self.seat.releasing() {
                out.supersede(self.seat()?)?;
                self.seat.release_on_landing();
            }
            return Ok(None);
        }
        if self.attached() {
            return self.send_playback(
                When::Next,
                vec![DeckPart::Detach { slot: self.seat()? }],
                out,
            );
        }
        self.status = TrackStatus::Released;
        self.release_lane(out)?;
        Ok(None)
    }

    pub(super) fn align(
        &mut self,
        to: Position,
        mut speed: SpeedCurve,
        at: SessionFrame,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        let to = self.raw_position(to)?;
        let change = TrackSettings::check(TrackSettingsChange::Speed(check_curve(&mut speed)?))?;
        let when = self.jump_when(at, out)?;
        let seq = self.send_speed_programme(
            vec![LaneCommand::SetSpeed(speed), LaneCommand::Jump { to }],
            when,
        )?;
        self.settings.track(seq, when, change);
        Ok(Some(seq))
    }

    pub(super) fn play(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_when(at, out)?;
        if matches!(self.status, TrackStatus::Ended { .. }) {
            if !self.adoptions.adopts(self.segment.id) {
                self.segment(Position::ZERO, None, out)?;
            }
            self.resume = None;
            self.play = Some(at);
            return Ok(None);
        }
        if let Some((resume, operation)) = self.pending_resume_command() {
            tracing::debug!(
                item = u64::from(self.item),
                seq = operation.seq.get(),
                current_segment = self.segment.id.get(),
                applied = ?operation.applied,
                requested_when = ?operation.when,
                paused_resume = ?resume,
                command = ?operation.command,
                "paused Play requires a new segment"
            );
            self.segment(resume.position, None, out)?;
            self.resume = None;
            self.play = Some(at);
            return Ok(None);
        }
        if !self.attached() || !self.segment.ready {
            self.play = Some(at);
            return Ok(None);
        }
        self.send_playback(
            at,
            vec![DeckPart::Start {
                slot: self.seat()?,
                fade: Fade::Declick,
            }],
            out,
        )
    }

    pub(super) fn pending_resume_command(&self) -> Option<(SlotMark, &LaneOperation)> {
        let resume = self.resume.or_else(|| {
            self.mark.filter(|_| {
                matches!(
                    self.status,
                    TrackStatus::Loaded | TrackStatus::Paused { .. }
                )
            })
        })?;
        if resume.lane.segment != self.segment.id {
            return None;
        }
        self.lane_commands
            .operations()
            .iter()
            .filter(|operation| match operation.when {
                When::At(frame) => {
                    frame.segment == resume.lane.segment && frame.frame > resume.lane.frame
                }
                When::Next | When::Deferred => operation.segment == resume.lane.segment,
            })
            .min_by_key(|operation| (operation.seq, operation.ordinal))
            .map(|operation| (resume, operation))
    }

    pub(super) fn send_playback(
        &mut self,
        at: When<SessionFrame>,
        parts: Vec<DeckPart>,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        let slot = self.seat()?;
        let action = parts
            .iter()
            .rev()
            .find_map(|part| PlaybackAction::try_from((part, slot)).ok());
        let basis = parts
            .iter()
            .flat_map(super::super::outbox::slots)
            .map(|slot| (slot, out.deck_basis(slot, at)))
            .collect();
        let seq = if at == When::Deferred {
            Some(out.deferred(parts)?)
        } else {
            out.deck(at, parts)?
        };
        if let Some(pending) = self.playback_commands.iter_mut().find(|operation| {
            seq.is_none() && operation.seq.is_none() && operation.segment == self.segment.id
        }) {
            if action.is_some() {
                pending.action = action;
            }
            for basis in basis {
                if !pending.basis.contains(&basis) {
                    pending.basis.push(basis);
                }
            }
        } else {
            self.playback_commands.push(PlaybackOperation {
                seq,
                segment: self.segment.id,
                basis,
                action,
                obsolete: false,
                transparent: None,
            });
        }
        Ok(seq)
    }

    pub(super) fn pending_playback_action(
        &self,
        slot: Slot,
        basis: Option<Seq>,
        query: PlaybackQuery,
    ) -> Option<(usize, PlaybackAction)> {
        let mut basis = basis;
        for (index, operation) in self.playback_commands.iter().enumerate().rev() {
            let staged = query == PlaybackQuery::Admission && operation.seq.is_none();
            let admitted = operation.seq.is_some()
                && (operation.seq == basis
                    || (operation.transparent.is_some() && operation.transparent == basis));
            if staged || admitted {
                if let Some(action) = operation.action {
                    return Some((index, action));
                }
                basis = operation.basis.iter().find(|(named, _)| *named == slot)?.1;
            }
        }
        None
    }

    pub(super) fn obsolete_playback_ancestors(&mut self, slot: Slot, mut basis: Option<Seq>) {
        for operation in self.playback_commands.iter_mut().rev() {
            if operation.seq.is_some()
                && (operation.seq == basis
                    || (operation.transparent.is_some() && operation.transparent == basis))
            {
                operation.obsolete = true;
                basis = operation
                    .basis
                    .iter()
                    .find(|(named, _)| *named == slot)
                    .and_then(|(_, basis)| *basis);
            }
        }
        if let Some(attach) = self.seat.attaching_mut()
            && attach.seq.is_some()
            && attach.seq == basis
        {
            attach.obsolete = true;
        }
    }
}
