use kithara_command::{Rejection, Seq, When};
use kithara_play::{Outbox, PlayError, Position, Settled, Track, TrackCommand, TrackStatus};
use kithara_signal::{FrameCount, SegmentId, SessionFrame};
use kithara_sync::{Slew, SlewCause, slew};

use super::{
    Linked, LinkedPlayer, Waiting,
    core::Refusal,
    geometry::cue_position,
    state::{Cause, CorrectionState, Cue, Reissued, Relocation, RunningCorrection, SpeedOperation},
};
use crate::{covers, jump_target};

/// How a correction closes the phase of a sounding deck.
pub(super) enum Corrected {
    /// By bounded speed steps.
    Slew(Option<Seq>),
    /// By relocating the deck, which holds the whole trajectory from there on.
    Move(Option<Seq>),
}

impl<P> Linked<P> {
    pub(super) fn drain_speed_receipts<S>(&mut self)
    where
        P: Track<S>,
    {
        if self.speeds.is_empty() {
            return;
        }
        while let Some(receipt) = self.inner.speed_receipt() {
            match receipt {
                Settled::Pending => {}
                Settled::Applied { seq, at } => self.speed_applied(seq, at),
                Settled::Rejected { seq, reason } => self.speed_rejected::<S>(seq, &reason),
            }
        }
    }

    /// An applied correction runs from `at`; any other applied speed is done.
    fn speed_applied(&mut self, seq: Seq, at: SessionFrame) {
        let Some((_, operation)) = self.speeds.iter_mut().find(|(pending, _)| *pending == seq)
        else {
            return;
        };
        match operation {
            SpeedOperation::Correction { state, .. } => {
                if let CorrectionState::Pending(correction) = state {
                    *state = CorrectionState::Applied(RunningCorrection { at, ..*correction });
                }
            }
            SpeedOperation::Alignment | SpeedOperation::Relocation { .. } => {
                self.speeds.retain(|(pending, _)| *pending != seq);
            }
        }
    }

    /// A synchronized track owes a refused speed's step again.
    fn speed_rejected<S>(&mut self, seq: Seq, reason: &Rejection<PlayError>)
    where
        P: Track<S>,
    {
        let Some(index) = self.speeds.iter().position(|(pending, _)| *pending == seq) else {
            return;
        };
        let (_, operation) = self.speeds.remove(index);
        if !self.synced() || !matches!(reason, Rejection::Late | Rejection::Refused(_)) {
            return;
        }

        match operation {
            SpeedOperation::Correction { cause, .. } => self.alignment = Some(cause),
            SpeedOperation::Alignment => {
                let required = self.inner.snapshot().as_ref().position;
                self.waiting = Some(Waiting::Sync { required });
            }
            SpeedOperation::Relocation { to } => {
                self.waiting = Some(Waiting::Seek { required: to });
                let caller = self
                    .relocation
                    .filter(|relocation| relocation.sent == Some(Reissued::Lane(seq)))
                    .map_or(seq, |relocation| relocation.caller);
                self.relocation = Some(Relocation { caller, sent: None });
            }
        }
    }

    pub(super) fn record_retime(&mut self, at: SessionFrame, sent: Option<Seq>) {
        let Some(seq) = sent else {
            return;
        };
        self.retimes.retain(|(frame, _)| *frame != at);
        self.retimes.push((at, seq));
    }

    pub(super) fn correct<S>(
        &mut self,
        at: SessionFrame,
        cause: Cause,
        out: &mut Outbox<'_, S>,
    ) -> Result<Corrected, PlayError>
    where
        P: Track<S>,
    {
        let Some(grid) = self.grid.as_ref() else {
            return Err(PlayError::NotReady);
        };
        let segment = self
            .inner
            .snapshot()
            .as_ref()
            .mark
            .ok_or(PlayError::NotReady)?
            .lane
            .segment;
        let (position, from) = self.planned::<S>(at)?;
        if !covers(grid.geometry(), position) {
            return Err(PlayError::NotReady);
        }
        if self.lane_room() == 0 {
            return Err(PlayError::Full("lane"));
        }

        let source = self.source(grid)?;
        let law = match cause {
            Cause::Commanded => SlewCause::Commanded,
            Cause::Drift => SlewCause::Drift {
                from,
                limit: self.config.slew_limit(),
            },
        };
        let planned = slew(
            &self.host,
            &source,
            position,
            at,
            self.config.epsilon(),
            law,
        );
        self.close_phase(planned, cause, at, segment, out)
    }

    /// Sends what closes the phase: the speed law of a slew, or a relocation.
    fn close_phase<S>(
        &mut self,
        planned: Option<Slew>,
        cause: Cause,
        at: SessionFrame,
        segment: SegmentId,
        out: &mut Outbox<'_, S>,
    ) -> Result<Corrected, PlayError>
    where
        P: Track<S>,
    {
        let (curve, frames) = match planned {
            Some(Slew::Closes { curve, frames }) => (curve, frames),
            Some(Slew::Relocate) | None if self.waiting.is_some() => {
                return Err(PlayError::NotReady);
            }
            Some(Slew::Relocate) => return self.align(out).map(Corrected::Move),
            None => {
                return Err(PlayError::Internal(
                    "source phase correction has no playable grid law".into(),
                ));
            }
        };

        let frames = usize::try_from(frames)
            .map(FrameCount::new)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let speed = TrackCommand::SetSpeed {
            speed: curve,
            at: When::At(at),
        };
        let sent = self.inner.apply(speed, out)?;
        if let Some(seq) = sent {
            let state = CorrectionState::Pending(RunningCorrection {
                at,
                frames,
                segment,
            });
            self.speeds
                .push((seq, SpeedOperation::Correction { cause, state }));
        }
        Ok(Corrected::Slew(sent))
    }

    /// Whether the owed correction moved the deck.
    pub(super) fn realign<S>(&mut self, out: &mut Outbox<'_, S>) -> bool
    where
        P: Track<S>,
    {
        let Some(cause) = self.alignment else {
            return false;
        };
        if !matches!(
            self.inner.snapshot().as_ref().status,
            TrackStatus::Playing { .. }
        ) {
            return false;
        }
        let result = self
            .earliest(out)
            .and_then(|at| self.correct(at, cause, out));
        match result {
            Ok(corrected) => {
                self.alignment = None;
                matches!(corrected, Corrected::Move(_))
            }
            Err(error) => {
                if let Refusal::Drop = self.triage(&error, out) {
                    self.alignment = None;
                }
                false
            }
        }
    }

    /// A sounding deck follows each of `frames` until one of them moves it; the
    /// landing already follows the rest of the trajectory.
    pub(super) fn retime_sounding<S>(&mut self, frames: &[SessionFrame], out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        for &at in frames {
            match self.correct(at, Cause::Commanded, out) {
                Ok(Corrected::Slew(sent)) => self.record_retime(at, sent),
                Ok(Corrected::Move(sent)) => {
                    self.record_retime(at, sent);
                    self.alignment = None;
                    return;
                }
                Err(error) => {
                    if let Refusal::Retry = self.triage(&error, out) {
                        self.alignment = Some(Cause::Commanded);
                        if matches!(error, PlayError::Late) && self.realign(out) {
                            return;
                        }
                    } else if !self.synced() {
                        return;
                    }
                }
            }
        }
    }

    /// An entry planned on the current grid owes that grid no correction.
    pub(super) fn entry_planned<S>(&mut self)
    where
        P: Track<S>,
    {
        if !matches!(
            self.inner.snapshot().as_ref().status,
            TrackStatus::Playing { .. }
        ) {
            self.alignment = None;
        }
    }

    fn align_playing<S>(
        &mut self,
        source: &kithara_sync::SourceGrid,
        position: Position,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        if self.lane_room() == 0 {
            return Err(PlayError::Full("lane"));
        }
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let to = jump_target(&self.host, source, position, at).ok_or(PlayError::NotReady)?;
        let start = at - self.inner.snapshot().as_ref().declick;
        let (before, _) = self.planned::<S>(start)?;
        let sent = self.inner.apply(
            TrackCommand::Align {
                to,
                speed: self.curve(source, before, start, Some((to, at)))?,
                at,
            },
            out,
        )?;
        if let Some(seq) = sent {
            self.speeds.push((seq, SpeedOperation::Alignment));
        }
        self.correction = None;
        self.waiting = None;
        Ok(sent)
    }

    pub(super) fn align<S>(&mut self, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let track = self.inner.snapshot();
        let required = cue_position(track.as_ref());
        if !matches!(track.as_ref().status, TrackStatus::Playing { .. }) {
            self.cue = Some(Cue::Covered(required));
            self.waiting.get_or_insert(Waiting::Sync { required });
            let answer = self.cue_on_beat(out);
            if let Err(error) = &answer
                && let Refusal::Unsyncable = self.triage(error, out)
            {
                return Ok(None);
            }
            return answer;
        }
        let Some(grid) = self.grid.as_ref() else {
            self.waiting = Some(Waiting::Sync { required });
            return Ok(None);
        };
        let source = self.source(grid)?;
        let at = self.jump_frame(out)?;
        let (position, _speed) = self.planned::<S>(at)?;
        if !covers(grid.geometry(), position) {
            self.waiting = Some(Waiting::Sync { required: position });
            return Ok(None);
        }
        self.align_playing(&source, position, at, out)
    }
}
