use kithara_audio::TrackFailureKind;
use kithara_command::{Rejection, Seq, When};
use kithara_config::{ConfigOwner, LiveConfig};
use kithara_render::{
    LaneCommand,
    bridge::{DeckPart, Fade, PlaybackFault, Slot},
};
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;

use super::{
    super::{
        outbox::Outbox,
        settings::{TrackSettings, TrackSettingsChange, TrackSettingsExec},
    },
    core::PlayerImpl,
    geometry::*,
    state::*,
};
use crate::PlayError;

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

impl<S> PlayerImpl<S> {
    pub(super) fn release_track(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.play = None;
        if let Some(loading) = self.loading.as_mut() {
            loading.speed = None;
        }
        if let Some(attaching) = self.attaching.as_mut() {
            attaching.play = None;
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
            .attaching
            .is_some_and(|attach| attach.replacement && attach.seq.is_some())
        {
            if !self.releasing {
                out.supersede(self.seat()?)?;
                self.releasing = true;
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
        self.releasing = false;
        self.release_lane(out)?;
        Ok(None)
    }

    pub(super) fn align(
        &mut self,
        to: Position,
        mut speed: SpeedCurve,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let to = self.raw_position(to)?;
        let change = TrackSettings::check(TrackSettingsChange::Speed(check_curve(&mut speed)?))?;
        let when = self.jump_when(at, out)?;
        let seq = self.send_lane_batch(
            vec![LaneCommand::SetSpeed(speed), LaneCommand::Jump { to }],
            when,
        )?;
        self.settings.track(seq, when, change);
        Ok(Some(seq))
    }

    pub(super) fn play(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_when(at, out)?;
        if matches!(self.status, TrackStatus::Ended { .. }) {
            self.segment(Position::ZERO, None, out)?;
            self.resume = None;
            self.play = Some(at);
            return Ok(None);
        }
        if let Some(resume) = self.resume.or_else(|| {
            self.mark.filter(|_| {
                matches!(
                    self.status,
                    TrackStatus::Loaded | TrackStatus::Paused { .. }
                )
            })
        }) && resume.lane.segment == self.segment
        {
            let pending = self
                .lane_commands
                .iter()
                .filter(|operation| operation.applied() != Some(false))
                .any(|operation| {
                    let pending = match operation.when {
                        When::At(frame) => {
                            frame.segment == resume.lane.segment && frame.frame > resume.lane.frame
                        }
                        When::Next | When::Deferred => operation.segment == resume.lane.segment,
                    };
                    if pending {
                        tracing::debug!(
                            item = u64::from(self.item),
                            seq = operation.seq.get(),
                            current_segment = self.segment.get(),
                            applied = ?operation.applied(),
                            requested_when = ?operation.when,
                            paused_resume = ?resume,
                            command = ?operation.command,
                            "paused Play requires a new segment"
                        );
                    }
                    pending
                });
            if pending {
                self.segment(resume.position, None, out)?;
                self.resume = None;
                self.play = Some(at);
                return Ok(None);
            }
        }
        if !self.attached() || self.ready != Some(self.segment) {
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

    pub(super) fn send_playback(
        &mut self,
        at: When<SessionFrame>,
        parts: Vec<DeckPart>,
        out: &mut Outbox<'_, S>,
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
            seq.is_none() && operation.seq.is_none() && operation.segment == self.segment
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
                segment: self.segment,
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
        if let Some(attach) = self.attaching.as_mut()
            && attach.seq.is_some()
            && attach.seq == basis
        {
            attach.obsolete = true;
        }
    }
}

impl<S> PlayerImpl<S> {
    pub(super) fn configuring(
        &self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<(Configuring, Option<usize>), PlayError> {
        if self.adopting.iter().any(|adoption| adoption.cancelled) {
            return Err(PlayError::NotReady);
        }
        if self.lane.is_none() {
            if !matches!(at, When::Next) {
                return Err(PlayError::Untimed);
            }
            return Ok((Configuring::Apply(TrackSettings::check(change)?), None));
        }
        if !matches!(self.status, TrackStatus::Playing { .. }) && at != When::Next {
            return Err(PlayError::Untimed);
        }
        let when = self.lane_when(at, out)?;
        let change = TrackSettings::check(change)?;
        if let TrackSettingsChange::Speed(speed) = change
            && matches!(when, When::Next)
            && self.unchanged_speed(speed)
        {
            return Ok((Configuring::Unchanged, None));
        }
        if matches!(change, TrackSettingsChange::Speed(_)) {
            let cost = self.speed_admission(at, out)?;
            return Ok((Configuring::Send(when, change), cost));
        }
        if self.lane.as_ref().is_none_or(|lane| lane.available() == 0) {
            return Err(PlayError::Full("lane"));
        }
        Ok((Configuring::Send(when, change), Some(0)))
    }

    fn unchanged_speed(&self, speed: f32) -> bool {
        speed == self.settings.config().speed()
            && self
                .loading
                .as_ref()
                .is_none_or(|loading| loading.speed.is_none())
            && !self.lane_commands.iter().any(|operation| {
                operation.applied().is_none()
                    && match operation.command {
                        LaneCommand::SetSpeed(_) => operation.segment == self.segment,
                        LaneCommand::Segment { id, .. } => id == self.segment,
                        _ => false,
                    }
            })
            && self
                .lane_commands
                .iter()
                .filter(|operation| {
                    operation.applied() == Some(true)
                        && match operation.command {
                            LaneCommand::SetSpeed(_) => operation.segment == self.segment,
                            LaneCommand::Segment { id, .. } => id == self.segment,
                            _ => false,
                        }
                })
                .max_by_key(|operation| (operation.when, operation.seq))
                .is_none_or(|operation| {
                    matches!(
                        operation.command,
                        LaneCommand::SetSpeed(SpeedCurve::Constant(_))
                            | LaneCommand::Segment {
                                speed: SpeedCurve::Constant(_),
                                ..
                            }
                    )
                })
    }

    pub(super) fn speed_admission(
        &self,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<Option<usize>, PlayError> {
        if self.lane.is_none() {
            return Err(PlayError::NotReady);
        }
        if !matches!(self.status, TrackStatus::Playing { .. }) {
            if at != When::Next {
                return Err(PlayError::Untimed);
            }
            if !self.attached()
                && self
                    .loading
                    .as_ref()
                    .is_some_and(|loading| loading.opened.is_none())
            {
                return Ok(None);
            }
            self.speed_position()?;
            self.check_segment(out)?;
            return Ok(Some(usize::from(self.attached())));
        }
        self.lane_when(at, out)?;
        if self.lane.as_ref().is_none_or(|lane| lane.available() == 0) {
            return Err(PlayError::Full("lane"));
        }
        Ok(Some(0))
    }

    pub(super) fn execute_speed(
        &mut self,
        speed: SpeedCurve,
        target: f32,
        when: When<kithara_render::LaneFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if !matches!(self.status, TrackStatus::Playing { .. }) {
            let unopened = !self.attached();
            if let Some(loading) = self
                .loading
                .as_mut()
                .filter(|loading| unopened && loading.opened.is_none())
            {
                loading.speed = Some((speed, target));
                return Ok(None);
            }
            return self.segment_with(self.speed_position()?, None, speed, out);
        }
        let seq = self.send_lane(LaneCommand::SetSpeed(speed), when)?;
        self.settings
            .track(seq, when, TrackSettingsChange::Speed(target));
        Ok(Some(seq))
    }

    pub(super) fn configure(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        match self.configuring(change, at, out)?.0 {
            Configuring::Apply(change) => {
                self.settings.apply(change)?;
                Ok(None)
            }
            Configuring::Unchanged => Ok(None),
            Configuring::Send(when, TrackSettingsChange::Speed(speed)) => {
                self.execute_speed(SpeedCurve::Constant(speed), speed, when, out)
            }
            Configuring::Send(when, change) => {
                let seq = self.send_lane(LaneCommand::from(change), when)?;
                self.settings.track(seq, when, change);
                Ok(Some(seq))
            }
        }
    }

    pub(super) fn preparation_closed(&self) -> bool {
        matches!(
            self.status,
            TrackStatus::Failed { .. } | TrackStatus::Released
        ) || self.preparation_failure().is_some()
    }

    pub(super) fn preparation_failure(&self) -> Option<TrackFailureKind> {
        if let TrackStatus::Failed {
            fault: PlaybackFault::Source(failure),
            ..
        } = self.status
        {
            return Some(failure);
        }
        let operation = self.lane_commands.iter().rev().find(|operation| {
            matches!(operation.command,
                LaneCommand::Segment { id, .. } | LaneCommand::SetHostRate { id, .. }
                if id == self.segment)
        })?;
        match operation.verdict {
            Some(Err(Rejection::Refused(failure))) => Some(failure),
            _ => None,
        }
    }
}

impl<'a, S> TrackSettingsExec<Outbox<'a, S>> for PlayerImpl<S> {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    fn exec_live(
        &mut self,
        change: TrackSettingsChange,
        at: Self::At,
        out: &mut Outbox<'a, S>,
    ) -> Self::Output {
        self.configure(change, at, out)
    }

    fn exec_speed(&mut self, speed: f32, at: Self::At, out: &mut Outbox<'a, S>) -> Self::Output {
        self.configure(TrackSettingsChange::Speed(speed), at, out)
    }
}
