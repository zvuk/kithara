use kithara_command::{Rejection, Seq, When};
use kithara_play::{Outbox, PlayError, Position, Settled, Track, TrackCommand, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::{Slew, SlewCause, SpeedCurve, slew};
use tracing::warn;

use super::{
    Linked, LinkedPlayer, Waiting,
    geometry::cue_position,
    state::{CorrectionState, Cue, Reissued, Relocation, RunningCorrection, SpeedOperation},
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
        if !self.speeds.is_empty() {
            while let Some(receipt) = self.inner.speed_receipt() {
                let seq = match &receipt {
                    Settled::Pending => continue,
                    Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => *seq,
                };
                let Some(index) = self.speeds.iter().position(|(pending, _)| *pending == seq)
                else {
                    continue;
                };
                if let Settled::Applied { at, .. } = receipt {
                    if let SpeedOperation::Correction { state, .. } = &mut self.speeds[index].1 {
                        if let CorrectionState::Pending(correction) = state {
                            *state =
                                CorrectionState::Applied(RunningCorrection { at, ..*correction });
                        }
                    } else {
                        self.speeds.remove(index);
                    }
                    continue;
                }
                let (_, operation) = self.speeds.remove(index);
                if self.synced()
                    && matches!(
                        receipt,
                        Settled::Rejected {
                            reason: Rejection::Late | Rejection::Refused(_),
                            ..
                        }
                    )
                {
                    match operation {
                        SpeedOperation::Correction { commanded, .. } => {
                            self.alignment = Some(commanded);
                        }
                        SpeedOperation::Alignment => {
                            self.waiting = Some(Waiting::Sync {
                                required: self.inner.snapshot().as_ref().position,
                            });
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
            }
        }
    }

    pub(super) fn record_retime(&mut self, at: SessionFrame, seq: Seq) {
        self.retimes.retain(|(frame, _)| *frame != at);
        self.retimes.push((at, seq));
    }

    pub(super) fn correct<S>(
        &mut self,
        at: SessionFrame,
        commanded: bool,
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
        let cause = if commanded {
            SlewCause::Commanded
        } else {
            SlewCause::Drift {
                from,
                limit: self.config.slew_limit(),
            }
        };

        match slew(
            &self.host,
            &source,
            position,
            at,
            self.config.epsilon(),
            cause,
        ) {
            Some(Slew::Closes { curve, frames }) => {
                let frames = usize::try_from(frames)
                    .map(FrameCount::new)
                    .map_err(|error| PlayError::Internal(error.to_string()))?;
                let correction = RunningCorrection {
                    at,
                    frames,
                    segment,
                };
                self.send_slew(curve, commanded, correction, out)
                    .map(Corrected::Slew)
            }
            Some(Slew::Relocate) | None if self.waiting.is_some() => Err(PlayError::NotReady),
            Some(Slew::Relocate) => self.align(out).map(Corrected::Move),
            None => Err(PlayError::Internal(
                "source phase correction has no playable grid law".into(),
            )),
        }
    }

    /// Sends the speed law of a slew and tracks its correction.
    fn send_slew<S>(
        &mut self,
        curve: SpeedCurve,
        commanded: bool,
        correction: RunningCorrection,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let sent = self.inner.apply(
            TrackCommand::SetSpeed {
                speed: curve,
                at: When::At(correction.at),
            },
            out,
        )?;

        if let Some(seq) = sent {
            self.speeds.push((
                seq,
                SpeedOperation::Correction {
                    commanded,
                    state: CorrectionState::Pending(correction),
                },
            ));
        }
        Ok(sent)
    }

    /// Whether the owed correction moved the deck.
    pub(super) fn realign<S>(&mut self, out: &mut Outbox<'_, S>) -> bool
    where
        P: Track<S>,
    {
        let Some(commanded) = self.alignment else {
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
            .and_then(|at| self.correct(at, commanded, out));
        match result {
            Ok(corrected) => {
                self.alignment = None;
                matches!(corrected, Corrected::Move(_))
            }
            Err(
                PlayError::Full(_) | PlayError::Late | PlayError::NotReady | PlayError::Untimed,
            ) => false,
            Err(error) => {
                warn!(%error, "track realignment refused");
                self.alignment = None;
                if matches!(error, PlayError::Internal(_)) {
                    self.refuse_sync(out);
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
            match self.correct(at, true, out) {
                Ok(Corrected::Slew(sent)) => {
                    if let Some(seq) = sent {
                        self.record_retime(at, seq);
                    }
                }
                Ok(Corrected::Move(sent)) => {
                    if let Some(seq) = sent {
                        self.record_retime(at, seq);
                    }
                    self.alignment = None;
                    return;
                }
                Err(PlayError::Late) => {
                    self.alignment = Some(true);
                    if self.realign(out) {
                        return;
                    }
                }
                Err(PlayError::Full(_) | PlayError::NotReady | PlayError::Untimed) => {
                    self.alignment = Some(true);
                }
                Err(error) => {
                    warn!(?at, %error, "track retime refused");
                    if matches!(error, PlayError::Internal(_)) {
                        self.refuse_sync(out);
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
            return match self.cue_on_beat(out) {
                Err(PlayError::Internal(error)) => {
                    warn!(%error, "track cannot follow the host");
                    self.refuse_sync(out);
                    Ok(None)
                }
                answer => answer,
            };
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
