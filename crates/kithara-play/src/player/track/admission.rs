use std::num::NonZeroU32;

use kithara_command::{Seq, When};
use kithara_render::{LaneFrame, bridge::DeckPart};
use kithara_signal::SessionFrame;
use kithara_warp::SpeedCurve;
use tracing::warn;

use super::{
    super::{
        factory::Track,
        outbox::{Outbox, Settled},
        settings::{TrackSettings, TrackSettingsChange},
    },
    core::PlayerImpl,
    geometry::*,
    state::*,
};
use crate::PlayError;

impl<S> Track<S> for PlayerImpl<S> {
    fn admit(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        self.observe(out);
        self.settle_lane();
        self.configuring(change, at, out).map(drop)
    }

    fn projected(&self) -> TrackSettings {
        self.settings.projected()
    }

    fn finish_group(&mut self, result: Result<Seq, &mut Vec<DeckPart>>) {
        match &result {
            Ok(seq) => {
                for operation in &mut self.playback_commands {
                    if operation.seq.is_none() {
                        operation.seq = Some(*seq);
                    }
                }
            }
            Err(_) => self
                .playback_commands
                .retain(|operation| operation.seq.is_some()),
        }
        let Some(attaching) = self.attaching.filter(|attach| attach.seq.is_none()) else {
            return;
        };
        match result {
            Ok(seq) => {
                if let Some(attach) = self.attaching.as_mut() {
                    attach.seq = Some(seq);
                    if attach.replacement {
                        attach.caller = seq;
                    }
                }
            }
            Err(parts) => {
                self.attaching = None;
                if self.restore_attachment(parts) {
                    if self.play.is_none() {
                        self.play = attaching.play;
                    }
                    if attaching.replacement {
                        self.slot = None;
                        self.reserved_ready();
                    }
                } else {
                    warn!("an aborted group did not return its original attachment PCM");
                }
            }
        }
    }

    fn planned(
        &self,
        at: SessionFrame,
        sample_rate: NonZeroU32,
    ) -> Result<(Position, f32), PlayError> {
        let mark = self.mark.ok_or(PlayError::Untimed)?;
        if mark.lane.segment != self.segment || !matches!(self.status, TrackStatus::Playing { .. })
        {
            return Err(PlayError::Untimed);
        }
        let elapsed = at.frames_since(mark.session).ok_or(PlayError::Untimed)?;
        let target = mark
            .lane
            .frame
            .checked_add(elapsed)
            .ok_or(PlayError::Untimed)?;
        let commands = self.planned_changes(mark)?;
        let base = SpeedCurve::Constant(self.segment_speed);
        let mut curve = &base;
        let mut origin = self.segment_speed;
        let mut start = 0;
        let mut cursor = mark.lane.frame;
        let mut position = mark.position.as_secs_f64();
        for (frame, _, change) in commands
            .into_iter()
            .filter(|(frame, _, _)| *frame <= target)
        {
            if frame > cursor {
                position += curve_area(
                    curve,
                    origin,
                    cursor.saturating_sub(start),
                    frame.saturating_sub(start),
                )? / f64::from(sample_rate.get());
                cursor = frame;
            }
            match change {
                PlannedChange::Speed(next) => {
                    origin = curve_speed(curve, origin, frame.saturating_sub(start))?;
                    start = frame;
                    curve = next;
                }
                PlannedChange::Jump(to) if frame > mark.lane.frame => position = to.as_secs_f64(),
                PlannedChange::Jump(_) => {}
            }
        }
        position += curve_area(
            curve,
            origin,
            cursor.saturating_sub(start),
            target.saturating_sub(start),
        )? / f64::from(sample_rate.get());
        let position = Position::try_from_secs_f64(position)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        Ok((
            position,
            curve_speed(curve, origin, target.saturating_sub(start))?,
        ))
    }

    fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError> {
        if let TrackStatus::Ended { at } = self.status {
            return Ok(Some(at));
        }
        let Some(duration) = self.duration else {
            return Ok(None);
        };
        if !matches!(self.status, TrackStatus::Playing { .. }) {
            return Ok(None);
        }
        let mark = self.mark.ok_or(PlayError::Untimed)?;
        if mark.lane.segment != self.segment {
            return Err(PlayError::Untimed);
        }
        let commands = self.planned_changes(mark)?;
        let base = SpeedCurve::Constant(self.segment_speed);
        let mut curve = &base;
        let mut origin = self.segment_speed;
        let mut start = 0;
        let mut cursor = mark.lane.frame;
        let rate = f64::from(sample_rate.get());
        let mut position = mark.position.as_secs_f64();
        for (frame, _, change) in commands {
            if frame > cursor {
                let remaining = (duration.as_secs_f64() - position).max(0.0) * rate;
                if let Some(offset) = curve_end(
                    curve,
                    origin,
                    cursor.saturating_sub(start),
                    remaining,
                    Some(frame.saturating_sub(start)),
                )? {
                    let end = start.checked_add(offset).ok_or(PlayError::Untimed)?;
                    return session_at(
                        mark,
                        LaneFrame {
                            segment: self.segment,
                            frame: end,
                        },
                    )
                    .map(Some)
                    .ok_or(PlayError::Untimed);
                }
                position += curve_area(
                    curve,
                    origin,
                    cursor.saturating_sub(start),
                    frame.saturating_sub(start),
                )? / rate;
                cursor = frame;
            }
            match change {
                PlannedChange::Speed(next) => {
                    origin = curve_speed(curve, origin, frame.saturating_sub(start))?;
                    start = frame;
                    curve = next;
                }
                PlannedChange::Jump(to) if frame > mark.lane.frame => position = to.as_secs_f64(),
                PlannedChange::Jump(_) => {}
            }
        }
        let remaining = (duration.as_secs_f64() - position).max(0.0) * rate;
        let offset = curve_end(curve, origin, cursor.saturating_sub(start), remaining, None)?
            .ok_or(PlayError::Untimed)?;
        let end = start.checked_add(offset).ok_or(PlayError::Untimed)?;
        session_at(
            mark,
            LaneFrame {
                segment: self.segment,
                frame: end,
            },
        )
        .map(Some)
        .ok_or(PlayError::Untimed)
    }

    fn speed_receipt(&mut self) -> Option<Settled> {
        self.settle_lane();
        let (index, settled) =
            self.speed_answers
                .iter()
                .enumerate()
                .find_map(|(index, (seq, outcome))| {
                    let settled = match outcome {
                        Ok((lane, session)) => {
                            let at = (*session)
                                .or_else(|| self.mark.and_then(|mark| session_at(mark, *lane)))?;
                            Settled::Applied { seq: *seq, at }
                        }
                        Err(reason) => Settled::Rejected {
                            seq: *seq,
                            reason: reason.clone(),
                        },
                    };
                    Some((index, settled))
                })?;
        self.speed_answers.remove(index);
        Some(settled)
    }

    fn speed_applied(&mut self, seq: Seq) -> Option<bool> {
        self.settle_lane();
        self.lane_commands
            .iter()
            .find(|operation| operation.seq == seq)
            .and_then(|operation| operation.applied)
    }

    fn cue(
        &mut self,
        position: Position,
        speed: f32,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.observe(out);
        self.settle_lane();
        self.segment_with(position, None, speed, out)?;
        Ok(self.lane_commands.last().map(|operation| operation.seq))
    }
}
