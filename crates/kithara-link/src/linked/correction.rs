use kithara_command::{Rejection, Seq, When};
use kithara_play::{Outbox, PlayError, Position, Settled, Track, TrackCommand, TrackStatus};
use kithara_signal::{FrameCount, SegmentId, SessionFrame};
use kithara_sync::{Slew, SlewCause, slew};

use super::{
    Linked, LinkedPlayer, Waiting,
    core::Refusal,
    geometry::cue_position,
    state::{Cause, CorrectionState, Cue, Reissued, RunningCorrection, SpeedOperation},
};
use crate::{covers, jump_target};

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

    /// Native success activates the new plan and retires the speed law it replaced.
    fn speed_applied(&mut self, seq: Seq, at: SessionFrame) {
        let Some((_, operation)) = self.speeds.iter_mut().find(|(pending, _)| *pending == seq)
        else {
            return;
        };
        let (coverage, keep_fence) = match operation {
            SpeedOperation::Correction { state, .. } => {
                if let CorrectionState::Pending(correction) = state {
                    *state = CorrectionState::Applied(RunningCorrection { at, ..*correction });
                }
                return;
            }
            SpeedOperation::Relocation {
                state, coverage, ..
            }
            | SpeedOperation::Alignment { state, coverage } => {
                if let Some(CorrectionState::Pending(correction)) = state {
                    let applied = *correction;
                    *state = Some(CorrectionState::Applied(applied));
                }
                (*coverage, state.is_some())
            }
        };
        self.speeds.retain(|(pending, operation)| {
            !(*pending < seq && matches!(operation, SpeedOperation::Correction { .. }))
                && (*pending != seq || keep_fence)
        });
        self.plan_sent(at, coverage);
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
        if !self.synced() {
            return;
        }
        let newer_seek = self.speeds.iter().any(|(pending, operation)| {
            *pending > seq && matches!(operation, SpeedOperation::Relocation { .. })
        });
        match (operation, reason) {
            (SpeedOperation::Correction { cause, .. }, Rejection::Late | Rejection::Refused(_))
                if !newer_seek =>
            {
                self.alignment = Some(cause);
            }
            (SpeedOperation::Alignment { .. }, Rejection::Late | Rejection::Refused(_)) => {
                let required = self.inner.snapshot().as_ref().position;
                self.waiting.get_or_insert(Waiting::Align { required });
            }
            (SpeedOperation::Relocation { .. }, Rejection::Late | Rejection::Stale) => {
                if let Some(relocation) = self
                    .relocation
                    .as_mut()
                    .filter(|relocation| relocation.sent == Some(Reissued::Lane(seq)))
                {
                    self.waiting = Some(Waiting::Seek {
                        required: relocation.required,
                    });
                    relocation.sent = None;
                }
            }
            _ => {}
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
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let Some(grid) = self.grid.as_ref() else {
            return Err(PlayError::NotReady);
        };
        let snapshot = self.inner.snapshot();
        let track = snapshot.as_ref();
        let segment = track.mark.ok_or(PlayError::NotReady)?.lane.segment;
        if self.speeds.iter().any(|(_, operation)| {
            matches!(operation, SpeedOperation::Relocation {
                state: Some(CorrectionState::Pending(landing) | CorrectionState::Applied(landing)), ..
            } | SpeedOperation::Alignment {
                state: Some(CorrectionState::Pending(landing) | CorrectionState::Applied(landing)), ..
            } if landing.segment == segment
                && at >= landing.at - track.declick && at < landing.at)
        }) {
            return Err(PlayError::NotReady);
        }
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
            Cause::Drift => SlewCause::Drift { from },
        };
        let planned = slew(
            &self.host,
            &source,
            position,
            at,
            self.config.epsilon(),
            law,
            self.horizon(),
        );
        self.close_phase(planned, cause, at, segment, out)
    }

    /// Sends a bounded correction without relocating the sounding track.
    fn close_phase<S>(
        &mut self,
        planned: Option<Slew>,
        cause: Cause,
        at: SessionFrame,
        segment: SegmentId,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let (curve, frames, coverage) = match planned {
            Some(Slew::Corrects {
                curve,
                frames,
                coverage,
            }) => (curve, frames, coverage),
            Some(Slew::Uncovered) => {
                return Err(PlayError::NotReady);
            }
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
            self.plan_sent(at, coverage);
        }
        Ok(sent)
    }

    /// A sounding deck owes a drift correction once the earliest frame it can
    /// command comes within half a horizon of its plan's end; in phase, that
    /// correction continues the same law.
    pub(super) fn renew_plan<S>(&mut self, out: &Outbox<'_, S>)
    where
        P: Track<S>,
    {
        let Some(end) = self.plan_end else {
            return;
        };
        if self.alignment.is_some()
            || !matches!(
                self.inner.snapshot().as_ref().status,
                TrackStatus::Playing { .. }
            )
        {
            return;
        }
        let Ok(half) = usize::try_from(self.horizon() / 2) else {
            return;
        };
        if self
            .earliest(out)
            .is_ok_and(|earliest| earliest + FrameCount::new(half) >= end)
        {
            self.alignment = Some(Cause::Drift);
        }
    }

    pub(super) fn realign<S>(&mut self, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        let Some(cause) = self.alignment else {
            return;
        };
        if !matches!(
            self.inner.snapshot().as_ref().status,
            TrackStatus::Playing { .. }
        ) {
            return;
        }
        let result = self
            .earliest(out)
            .and_then(|at| self.correct(at, cause, out));
        match result {
            Ok(_) => self.alignment = None,
            Err(error) => {
                if let Refusal::Drop = self.triage(&error, out) {
                    self.alignment = None;
                }
            }
        }
    }

    /// A sounding deck follows each commanded frame without changing its position.
    pub(super) fn retime_sounding<S>(&mut self, frames: &[SessionFrame], out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        for &at in frames {
            match self.correct(at, Cause::Commanded, out) {
                Ok(sent) => self.record_retime(at, sent),
                Err(error) => {
                    if let Refusal::Retry = self.triage(&error, out) {
                        self.alignment = Some(Cause::Commanded);
                        if matches!(error, PlayError::Late) {
                            self.realign(out);
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
        let (speed, coverage) = self.curve(source, before, start, Some((to, at)))?;
        let mark = self.inner.snapshot().as_ref().mark;
        let sent = self
            .inner
            .apply(TrackCommand::Align { to, speed, at }, out)?;
        if let Some(seq) = sent {
            let state = mark.map(|mark| {
                CorrectionState::Pending(RunningCorrection {
                    at,
                    frames: FrameCount::new(0),
                    segment: mark.lane.segment,
                })
            });
            self.speeds
                .push((seq, SpeedOperation::Alignment { state, coverage }));
        }
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
            self.waiting = Some(Waiting::Align { required: position });
            return Ok(None);
        }
        self.align_playing(&source, position, at, out)
    }
}
