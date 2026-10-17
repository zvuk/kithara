use std::num::{NonZeroU32, NonZeroU128};

use kithara_command::{Seq, When};
use kithara_config::LiveConfig;
use kithara_render::{
    LaneCommand, LaneFrame,
    bridge::{DeckPart, SlotMark},
};
use kithara_signal::{AudioChunkInfo, SessionFrame, SourceSpan};
use kithara_sync::SpeedCurve;
use kithara_warp::SourceTrajectory;
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
use crate::{Bound, PlayError, Player};

impl<S> Track<S> for PlayerImpl<S> {
    fn admit(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<Option<usize>, PlayError> {
        self.observe(out);
        self.settle_lane();
        if let Some(failure) = self.preparation_failure() {
            return Err(PlayError::Lane(failure));
        }
        self.configuring(change, at, out).map(|(_, cost)| cost)
    }

    fn projected(&self) -> TrackSettings {
        let mut settings = self.settings.projected();
        if let Some((_, target)) = self
            .loading
            .as_ref()
            .and_then(|loading| loading.speed.as_ref())
        {
            settings.apply_change(TrackSettingsChange::Speed(*target));
        }
        settings
    }

    fn prepared_speed(&self) -> Option<&SpeedCurve> {
        let consumed = self
            .resume
            .or(self.mark)
            .is_some_and(|mark| mark.lane.segment == self.segment && mark.lane.frame > 0);
        let mut prepared = None;
        for operation in self.lane_commands.iter().rev() {
            if operation.applied() == Some(false) {
                continue;
            }
            match &operation.command {
                LaneCommand::Segment { id, speed, .. } if *id == self.segment => {
                    prepared = Some(speed);
                    break;
                }
                LaneCommand::SetSpeed(speed) if operation.segment == self.segment => {
                    prepared = match operation.when {
                        When::At(frame) if frame.segment == self.segment && frame.frame == 0 => {
                            Some(speed)
                        }
                        _ => None,
                    };
                    break;
                }
                _ => {}
            }
        }
        prepared.filter(|curve| {
            !consumed
                || match curve {
                    SpeedCurve::Constant(_) => true,
                    SpeedCurve::Approach(curve) => {
                        curve.time_constant() == 0.0 || curve.initial() == curve.target()
                    }
                    _ => false,
                }
        })
    }

    fn prepare_entry(
        &mut self,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<bool, PlayError> {
        self.observe(out);
        self.settle_lane();
        if let Some(failure) = self.preparation_failure() {
            return Err(PlayError::Lane(failure));
        }
        if matches!(self.status, TrackStatus::Ended { .. })
            && self
                .adopting
                .iter()
                .any(|adoption| adoption.segment == self.segment)
        {
            return Ok(false);
        }
        Ok(self.entry(Bound::AtOrAfter(at)).is_some())
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
    ) -> Result<(Position, f64), PlayError> {
        let media_origin = self.media_origin.ok_or(PlayError::Untimed)?;
        let (mark, target) = self.planned_target(at)?;
        let commands = self.planned_changes(mark.lane.segment, true)?;
        let base = SpeedCurve::Constant(self.segment_speed);
        let mut curve = &base;
        let mut origin = f64::from(self.segment_speed);
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
        let position =
            Position::try_from_secs_f64((position - media_origin.as_secs_f64()).max(0.0))
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
        let Some(endpoint) = self
            .duration
            .and_then(|duration| self.media_origin?.checked_add(duration))
        else {
            return Ok(None);
        };
        if !matches!(self.status, TrackStatus::Playing { .. }) {
            return Ok(None);
        }
        let (mark, span, offset) = self.planned_span()?;
        let basis = self.source_basis.ok_or(PlayError::Untimed)?;
        let endpoint = source_endpoint(basis, endpoint, sample_rate)?;
        let commands = self.planned_changes(mark.lane.segment, true)?;
        let mut trajectory = self.trajectory_at(mark, span, offset, sample_rate, &commands)?;
        let mut cursor = mark.lane.frame;
        for (at, _, change) in commands.iter().filter(|(at, _, _)| *at > mark.lane.frame) {
            if let Some(offset) = trajectory
                .first_at_or_after(0, sample_rate, endpoint, Some(at - cursor))
                .map_err(|error| PlayError::Internal(error.to_string()))?
            {
                return self.planned_session_end(mark, cursor, offset);
            }
            trajectory
                .advance_by(0, sample_rate, at - cursor)
                .map_err(|error| PlayError::Internal(error.to_string()))?;
            cursor = *at;
            project_change(&mut trajectory, change, sample_rate)?;
        }
        let offset = trajectory
            .first_at_or_after(0, sample_rate, endpoint, None)
            .map_err(|error| PlayError::Internal(error.to_string()))?
            .ok_or(PlayError::Untimed)?;
        self.planned_session_end(mark, cursor, offset)
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
            .and_then(LaneOperation::applied)
    }

    fn cue(
        &mut self,
        position: Position,
        speed: SpeedCurve,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.observe(out);
        self.settle_lane();
        let position = self.raw_position(position)?;
        self.segment_with(position, None, speed, out)?;
        Ok(self.lane_commands.last().map(|operation| operation.seq))
    }
}

impl<S> PlayerImpl<S> {
    fn planned_target(&self, at: SessionFrame) -> Result<(SlotMark, u64), PlayError> {
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
        Ok((mark, target))
    }

    fn planned_span(&self) -> Result<(SlotMark, SourceSpan, u64), PlayError> {
        let mark = self.mark.ok_or(PlayError::Untimed)?;
        let source = self.observed_source.ok_or(PlayError::Untimed)?;
        if mark.lane.segment != self.segment || source.segment != self.segment {
            return Err(PlayError::Untimed);
        }
        let span = source
            .source_geometry()
            .ok_or_else(|| PlayError::Internal("invalid PCM source geometry".into()))?;
        let offset = mark
            .lane
            .frame
            .checked_sub(source.lane_frame)
            .filter(|offset| *offset <= span.output_frames())
            .ok_or(PlayError::Untimed)?;
        if span.position_at(offset) != Some(mark.position)
            || span.render_revision() != source.render_revision
            || span.mapping_revision() != source.mapping_revision
        {
            return Err(PlayError::Untimed);
        }
        Ok((mark, span, offset))
    }

    fn trajectory_at(
        &self,
        mark: SlotMark,
        span: SourceSpan,
        offset: u64,
        sample_rate: NonZeroU32,
        commands: &[(u64, Seq, PlannedChange<'_>)],
    ) -> Result<SourceTrajectory, PlayError> {
        let mut trajectory = SourceTrajectory::new(self.segment_speed);
        let mut cursor = 0;
        for (at, _, change) in commands.iter().filter(|(at, _, _)| *at <= mark.lane.frame) {
            trajectory
                .advance_by(0, sample_rate, at - cursor)
                .map_err(|error| PlayError::Internal(error.to_string()))?;
            cursor = *at;
            project_change(&mut trajectory, change, sample_rate)?;
        }
        trajectory
            .advance_by(0, sample_rate, mark.lane.frame - cursor)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        trajectory
            .reanchor(0, sample_rate, source_ratio(span, offset, sample_rate)?)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        Ok(trajectory)
    }

    fn planned_session_end(
        &self,
        mark: SlotMark,
        cursor: u64,
        offset: u64,
    ) -> Result<Option<SessionFrame>, PlayError> {
        let end = cursor.checked_add(offset).ok_or(PlayError::Untimed)?;
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
}

fn source_ratio(
    span: SourceSpan,
    offset: u64,
    rate: NonZeroU32,
) -> Result<(u128, NonZeroU128), PlayError> {
    span.scaled_from(
        (0, NonZeroU128::MIN),
        (0, NonZeroU128::MIN),
        (
            u128::from(rate.get()),
            NonZeroU128::from(span.sample_rate()),
        ),
    )
    .and_then(|span| span.source_ratio_at(offset))
    .ok_or(PlayError::Untimed)
}

fn source_endpoint(
    basis: AudioChunkInfo,
    endpoint: Position,
    rate: NonZeroU32,
) -> Result<(u128, NonZeroU128), PlayError> {
    let (numerator, denominator) = kithara_audio::source_lattice_position(&basis, endpoint)
        .map_err(|error| PlayError::Internal(error.to_string()))?;
    let point = SourceSpan::try_from((
        numerator,
        denominator.get(),
        denominator,
        basis.spec.sample_rate,
        1,
    ))
    .map_err(|()| PlayError::Untimed)?;
    source_ratio(point, 0, rate)
}

fn project_change(
    trajectory: &mut SourceTrajectory,
    change: &PlannedChange<'_>,
    sample_rate: NonZeroU32,
) -> Result<(), PlayError> {
    match change {
        PlannedChange::Speed(next) => {
            trajectory
                .replace((*next).clone())
                .map_err(|error| PlayError::Internal(error.to_string()))?;
        }
        PlannedChange::Jump(to) => {
            trajectory.reset();
            let numerator = to
                .as_nanos()
                .checked_mul(u128::from(sample_rate.get()))
                .ok_or(PlayError::Untimed)?;
            let denominator = NonZeroU128::new(1_000_000_000).ok_or(PlayError::Untimed)?;
            trajectory
                .reanchor(0, sample_rate, (numerator, denominator))
                .map_err(|error| PlayError::Internal(error.to_string()))?;
        }
    }
    Ok(())
}
