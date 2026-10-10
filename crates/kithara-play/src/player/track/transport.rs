use kithara_command::{Outcome, Seq, When};
use kithara_config::{ConfigOwner, LiveConfig};
use kithara_render::{
    LaneCommand,
    bridge::{DeckPart, Fade},
};
use kithara_signal::SessionFrame;
use kithara_warp::SpeedCurve;
use tracing::warn;

use super::{
    super::{
        outbox::{Outbox, rejection},
        settings::{TrackSettings, TrackSettingsChange, TrackSettingsExec},
    },
    core::PlayerImpl,
    geometry::*,
    state::*,
};
use crate::PlayError;

impl<S> PlayerImpl<S> {
    pub(super) fn release_track(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.play = None;
        self.playback_commands.clear();
        if let Some(attaching) = self.attaching.as_mut() {
            attaching.play = None;
        }
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
        self.release_lane(out)?;
        Ok(None)
    }

    pub(super) fn retry_detach(&mut self, out: &mut Outbox<'_, S>) {
        if let Some(slot) = self.slot
            && let Err(error) = self.send_playback(When::Next, vec![DeckPart::Detach { slot }], out)
        {
            warn!(%error, "released replacement waits to detach");
        }
    }

    pub(super) fn play(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_when(at, out)?;
        if let Some(resume) = self.resume {
            let pending = self
                .lane_commands
                .iter()
                .filter(|operation| operation.applied != Some(false))
                .any(|operation| match operation.when {
                    When::At(frame) => {
                        frame.segment == resume.lane.segment && frame.frame > resume.lane.frame
                    }
                    When::Next | When::Deferred => operation.segment == resume.lane.segment,
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
            });
        }
        Ok(seq)
    }

    pub(super) fn configuring(
        &self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<Configuring, PlayError> {
        if self.adopting.iter().any(|adoption| adoption.cancelled) {
            return Err(PlayError::NotReady);
        }
        if self.lane.is_none() {
            if !matches!(at, When::Next) {
                return Err(PlayError::Untimed);
            }
            return Ok(Configuring::Apply(TrackSettings::check(change)?));
        }
        let when = self.lane_when(at, out)?;
        let change = TrackSettings::check(change)?;
        if let TrackSettingsChange::Speed(speed) = change
            && matches!(when, When::Next)
            && speed == self.settings.config().speed()
            && !self.lane_commands.iter().any(|operation| {
                operation.segment == self.segment
                    && operation.applied.is_none()
                    && matches!(operation.command, LaneCommand::SetSpeed(_))
            })
            && self
                .lane_commands
                .iter()
                .filter(|operation| {
                    operation.segment == self.segment
                        && operation.applied == Some(true)
                        && matches!(operation.command, LaneCommand::SetSpeed(_))
                })
                .max_by_key(|operation| (operation.when, operation.seq))
                .is_none_or(|operation| {
                    matches!(
                        operation.command,
                        LaneCommand::SetSpeed(SpeedCurve::Constant(_))
                    )
                })
        {
            return Ok(Configuring::Unchanged);
        }
        if self.lane.as_ref().is_none_or(|lane| lane.available() == 0) {
            return Err(PlayError::Full("lane"));
        }
        Ok(Configuring::Send(when, change))
    }

    pub(super) fn configure(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        match self.configuring(change, at, out)? {
            Configuring::Apply(change) => {
                self.settings.apply(change)?;
                Ok(None)
            }
            Configuring::Unchanged => Ok(None),
            Configuring::Send(when, change) => {
                let seq = self.send_lane(LaneCommand::from(change), when)?;
                self.settings.track(seq, when, change);
                Ok(Some(seq))
            }
        }
    }

    pub(super) fn settle_lane(&mut self) {
        let Some(lane) = self.lane.as_mut() else {
            return;
        };
        for receipt in lane.receipts() {
            self.settings.settle(&receipt);
            let operation = self
                .lane_commands
                .iter()
                .position(|operation| operation.seq == receipt.seq());
            if let Outcome::Applied { at, data } = receipt.outcome() {
                if let Some(index) = operation {
                    let operation = &mut self.lane_commands[index];
                    let session = match operation.when {
                        When::At(requested) if requested == *at => operation.session,
                        _ => self.mark.and_then(|mark| session_at(mark, *at)),
                    };
                    operation.when = When::At(*at);
                    operation.applied = Some(true);
                    if matches!(&operation.command, LaneCommand::SetSpeed(_)) {
                        self.speed_answers
                            .push_back((receipt.seq(), Ok((*at, session))));
                    }
                }
                self.engine_latency = data.engine_latency;
                if let Some(ready) = data.ready {
                    self.ready = Some(ready);
                }
            } else if let Outcome::Rejected(reason) = receipt.outcome() {
                if let Some(index) = operation {
                    let operation = &mut self.lane_commands[index];
                    operation.applied = Some(false);
                    if matches!(operation.command, LaneCommand::SetSpeed(_)) {
                        self.speed_answers.push_back((
                            receipt.seq(),
                            Err(rejection(reason, |never| match *never {})),
                        ));
                    }
                }
                warn!(?reason, "lane command was rejected");
            }
        }
        self.reserved_ready();
    }
}

impl<S> TrackSettingsExec<()> for PlayerImpl<S> {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    fn exec_live(
        &mut self,
        change: TrackSettingsChange,
        at: Self::At,
        _cx: &mut (),
    ) -> Self::Output {
        if !matches!(at, When::Next) {
            return Err(PlayError::Untimed);
        }
        if self.lane.is_some() {
            let change = TrackSettings::check(change)?;
            let seq = self.send_lane(LaneCommand::from(change), When::Next)?;
            self.settings.track(seq, When::Next, change);
            Ok(Some(seq))
        } else {
            self.settings.apply(change)?;
            Ok(None)
        }
    }

    fn exec_speed(&mut self, speed: f32, at: Self::At, cx: &mut ()) -> Self::Output {
        self.exec_live(TrackSettingsChange::Speed(speed), at, cx)
    }
}
