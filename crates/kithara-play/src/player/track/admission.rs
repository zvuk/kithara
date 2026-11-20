use std::{
    collections::HashSet,
    num::{NonZeroU32, NonZeroU128},
};

use kithara_command::{Sender, Seq, When};
use kithara_config::{ConfigOwner, LiveConfig};
use kithara_render::{
    LaneCommand, LaneFrame,
    bridge::{DeckPart, SlotMark},
};
use kithara_signal::{AudioChunkInfo, SegmentId, SessionFrame, SourceSpan};
use kithara_sync::SpeedCurve;
use kithara_warp::SourceTrajectory;
use num_traits::ToPrimitive;
use tracing::warn;

use super::{
    super::{
        factory::Track,
        outbox::{Outbox, Settled},
        settings::{TrackSettings, TrackSettingsChange},
    },
    core::PlayerImpl,
    geometry::*,
    seat::Seat,
    state::*,
};
use crate::{Bound, PlayError, Player};

impl Track for PlayerImpl {
    fn admit(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_>,
    ) -> Result<Option<usize>, PlayError> {
        self.observe(out);
        self.settle_lane();
        if let Some(failure) = self.preparation_failure() {
            return Err(PlayError::Lane(failure));
        }
        self.configuring(change, at, out).map(|(_, cost)| cost)
    }

    fn projected(&self) -> TrackSettings {
        self.projected_settings()
    }

    fn prepared_speed(&self) -> Option<&SpeedCurve> {
        if self.pending_resume_command().is_some() {
            return None;
        }
        let consumed = self
            .resume
            .or(self.mark)
            .is_some_and(|mark| mark.lane.segment == self.segment.id && mark.lane.frame > 0);
        let operation = self
            .lane_commands
            .operations()
            .iter()
            .filter(|operation| match operation.command {
                LaneCommand::Segment { id, .. } => id == self.segment.id,
                LaneCommand::SetSpeed(_) => operation.segment == self.segment.id,
                _ => false,
            })
            .max_by_key(|operation| (operation.seq, operation.ordinal))?;
        let prepared = match &operation.command {
            LaneCommand::Segment { speed, .. } => Some(speed),
            LaneCommand::SetSpeed(speed) => match operation.when {
                When::At(at) if at.segment == self.segment.id && at.frame == 0 => Some(speed),
                _ => None,
            },
            _ => None,
        };
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

    fn prepare_entry(&mut self, at: SessionFrame, out: &mut Outbox<'_>) -> Result<bool, PlayError> {
        self.observe(out);
        self.settle_lane();
        if let Some(failure) = self.preparation_failure() {
            return Err(PlayError::Lane(failure));
        }
        if matches!(self.status, TrackStatus::Ended { .. })
            && self.adoptions.adopts(self.segment.id)
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
        let Some(attaching) = self
            .seat
            .attaching()
            .copied()
            .filter(|attach| attach.seq.is_none())
        else {
            return;
        };
        match result {
            Ok(seq) => {
                if let Some(attach) = self.seat.attaching_mut() {
                    attach.seq = Some(seq);
                    if attach.replacement {
                        attach.caller = seq;
                    }
                }
            }
            Err(parts) => {
                self.seat.vacate();
                if self.restore_attachment(parts) {
                    if self.play.is_none() {
                        self.play = attaching.play;
                    }
                    if attaching.replacement {
                        self.seat = Seat::Unseated;
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
        let media_origin = self.extent.media_origin.ok_or(PlayError::Untimed)?;
        let (mark, target) = self.planned_target(at)?;
        let commands = self.lane_changes(true)?;
        let base = SpeedCurve::Constant(self.segment.speed);
        let mut curve = &base;
        let mut origin = f64::from(self.segment.speed);
        let mut start = 0;
        let mut cursor = mark.lane.frame;
        let mut position = mark.position.as_secs_f64();
        for (frame, _, change) in commands.iter().filter(|(frame, _, _)| *frame <= target) {
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

    fn planned_law(&self, at: SessionFrame) -> Result<SpeedCurve, PlayError> {
        let (_, target) = self.planned_target(at)?;
        let commands = self.lane_changes(true)?;
        let base = SpeedCurve::Constant(self.segment.speed);
        let mut curve = &base;
        let mut origin = f64::from(self.segment.speed);
        let mut start = 0;
        let mut law = RestatedLaw::default();
        for (frame, _, change) in commands.iter() {
            let PlannedChange::Speed(next) = change else {
                continue;
            };
            if frame > target && law.is_empty() {
                law.restate(curve, origin, target.saturating_sub(start), 0)?;
            }
            origin = curve_speed(curve, origin, frame.saturating_sub(start))?;
            start = frame;
            curve = next;
            if frame > target {
                law.restate(curve, origin, 0, frame - target)?;
            }
        }
        if law.is_empty() {
            law.restate(curve, origin, target.saturating_sub(start), 0)?;
        }
        law.finish()
    }

    fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError> {
        if let TrackStatus::Ended { at } = self.status {
            return Ok(Some(at));
        }
        let Some(endpoint) = self
            .extent
            .duration
            .and_then(|duration| self.extent.media_origin?.checked_add(duration))
        else {
            return Ok(None);
        };
        if !matches!(self.status, TrackStatus::Playing { .. }) {
            return Ok(None);
        }
        let (mark, span, offset) = self.planned_span()?;
        let basis = self.source_basis.ok_or(PlayError::Untimed)?;
        let endpoint = source_endpoint(basis, endpoint, sample_rate)?;
        let commands = self.lane_changes(true)?;
        let plan = EndPlan {
            source: (
                self.observed_source.map(|source| source.source_revision),
                span.render_revision(),
                span.mapping_revision(),
            ),
            basis,
            endpoint,
            speed: self.segment.speed.to_bits(),
            sample_rate,
            declick: self.geometry.declick,
        };
        let known = self.lane_commands.cached_end(&plan, mark.lane.frame);
        let end = if let Some(end) = known {
            end
        } else {
            let end = self.project_end(mark, span, offset, &plan, &commands)?;
            self.lane_commands.cache_end(plan, end);
            end
        };
        self.planned_session_end(mark, end)
    }

    fn lane_receipt(&mut self) -> Option<Settled<LaneFrame>> {
        self.lane_commands.receipts.pop_front()
    }

    fn seek_applied(&mut self, seq: Seq) -> Option<bool> {
        self.settle_lane();
        self.lane_commands
            .operations()
            .iter()
            .find(|operation| operation.seq == seq)
            .map_or(Some(false), |operation| operation.applied.then_some(true))
    }

    fn source_request(&self) -> Option<Seq> {
        match &self.lane {
            Lane::Opening(_) | Lane::RetiringOpen(_) => self.loading.as_ref().map(|load| load.seq),
            _ => self.lane.release_seq(),
        }
    }

    fn source_closed(&self) -> bool {
        matches!(self.lane, Lane::None)
    }

    fn attached(&self) -> bool {
        Self::attached(self)
    }

    fn cue(
        &mut self,
        position: Position,
        speed: SpeedCurve,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        self.observe(out);
        self.settle_lane();
        let position = self.raw_position(position)?;
        self.segment_with(position, None, speed, out)?;
        Ok(self.segment.request_seq())
    }
}

impl PlayerImpl {
    pub(super) fn projected_settings(&self) -> TrackSettings {
        let operations = self.lane_commands.operations();
        let mut current = HashSet::new();
        if operations
            .iter()
            .any(|operation| !operation.is_projected(&current))
        {
            current.extend(
                self.lane
                    .sender()
                    .into_iter()
                    .flat_map(Sender::current_pending),
            );
        }
        let mut pending: Vec<_> = self.settings.pending().collect();
        pending.sort_by_key(|&(seq, when, _)| (when, seq));
        let mut settings = *self.settings.config();
        for (seq, _, change) in pending {
            if operations
                .iter()
                .any(|operation| operation.seq == seq && operation.is_projected(&current))
            {
                settings.apply_change(change);
            }
        }
        if let Some((_, target)) = self
            .loading
            .as_ref()
            .and_then(|loading| loading.speed.as_ref())
        {
            settings.apply_change(TrackSettingsChange::Speed(*target));
        }
        settings
    }

    pub(super) fn consumed_speed(&self) -> Result<f32, PlayError> {
        let frame = match self
            .resume
            .or(self.mark)
            .filter(|mark| mark.lane.segment == self.segment.id)
        {
            Some(mark) => mark.lane.frame,
            None if matches!(
                self.status,
                TrackStatus::Loading | TrackStatus::Loaded | TrackStatus::Paused { .. }
            ) =>
            {
                0
            }
            None => return Err(PlayError::Untimed),
        };
        let mut commands = self.lane_changes(false)?;
        let through = commands.operations.partition_point(|operation| {
            operation
                .frame_for(commands.segment)
                .is_some_and(|at| at <= frame)
        });
        commands.operations = &commands.operations[..through];
        let speeds = commands
            .indexed()
            .filter_map(|(at, seq, ordinal, change)| match change {
                PlannedChange::Speed(curve) => Some((at, seq, ordinal, curve)),
                PlannedChange::Jump(_) => None,
            });
        let anchor = speeds
            .clone()
            .rev()
            .find(|(_, _, _, curve)| match curve {
                SpeedCurve::Constant(_)
                | SpeedCurve::Approach(_)
                | SpeedCurve::ApproachSteps(_) => true,
                SpeedCurve::Steps(steps) => steps.first().is_some_and(|(at, _)| *at == 0),
                SpeedCurve::Ramp { .. } => false,
            })
            .map(|(at, seq, ordinal, _)| (at, seq, ordinal));
        let base = SpeedCurve::Constant(self.segment.speed);
        let mut curve = &base;
        let mut origin = f64::from(self.segment.speed);
        let mut start = 0;
        for (at, _, _, next) in speeds.skip_while(|(at, seq, ordinal, _)| {
            anchor.is_some_and(|anchor| (*at, *seq, *ordinal) < anchor)
        }) {
            origin = curve_speed(curve, origin, at.saturating_sub(start))?;
            curve = next;
            start = at;
        }
        curve_speed(curve, origin, frame.saturating_sub(start))?
            .to_f32()
            .ok_or(PlayError::Untimed)
    }

    fn lane_changes(&self, future: bool) -> Result<LaneChanges<'_>, PlayError> {
        let segment = self.segment.id;
        let declick = self
            .geometry
            .declick
            .get()
            .to_u64()
            .ok_or(PlayError::Untimed)?;
        let operations = self.lane_commands.operations();
        let mut current = HashSet::new();
        if operations
            .iter()
            .any(|operation| !operation.is_projected(&current))
        {
            current.extend(
                self.lane
                    .sender()
                    .into_iter()
                    .flat_map(Sender::current_pending),
            );
        }
        let first = operations.partition_point(|operation| operation.frame_for(segment).is_none());
        let known = &operations[first..];
        let maximum = u64::MAX - declick;
        let overflow = known.partition_point(|operation| {
            operation.frame_for(segment).is_some_and(|at| at <= maximum)
        });
        let unresolved = operations[..first]
            .iter()
            .take(if future { first } else { 0 })
            .filter(|operation| operation.is_projected(&current))
            .filter(|operation| {
                operation.segment == segment
                    && matches!(operation.when, When::Next | When::Deferred)
                    && matches!(
                        operation.command,
                        LaneCommand::SetSpeed(_) | LaneCommand::Jump { .. }
                    )
            })
            .map(|operation| ((operation.seq, operation.ordinal), PlayError::NotReady));
        let oversized = known[overflow..]
            .iter()
            .filter(|operation| operation.is_projected(&current))
            .filter(|operation| matches!(operation.command, LaneCommand::Jump { .. }))
            .map(|operation| ((operation.seq, operation.ordinal), PlayError::Untimed));
        if let Some((_, error)) = unresolved.chain(oversized).min_by_key(|(order, _)| *order) {
            return Err(error);
        }
        Ok(LaneChanges {
            operations: known,
            current,
            segment,
            declick,
        })
    }

    fn planned_target(&self, at: SessionFrame) -> Result<(SlotMark, u64), PlayError> {
        let mark = self.mark.ok_or(PlayError::Untimed)?;
        if mark.lane.segment != self.segment.id
            || !matches!(self.status, TrackStatus::Playing { .. })
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
        if mark.lane.segment != self.segment.id || source.segment != self.segment.id {
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

    /// Walks the accepted plan from the observed mark to the lane frame
    /// where the source reaches its endpoint.
    fn project_end(
        &self,
        mark: SlotMark,
        span: SourceSpan,
        offset: u64,
        plan: &EndPlan,
        commands: &LaneChanges<'_>,
    ) -> Result<u64, PlayError> {
        let sample_rate = plan.sample_rate;
        let mut trajectory = self.trajectory_at(mark, span, offset, sample_rate, commands)?;
        let mut cursor = mark.lane.frame;
        for (at, _, change) in commands.iter().filter(|(at, _, _)| *at > mark.lane.frame) {
            if let Some(offset) = trajectory
                .first_at_or_after(0, sample_rate, plan.endpoint, Some(at - cursor))
                .map_err(|error| PlayError::Internal(error.to_string()))?
            {
                return cursor.checked_add(offset).ok_or(PlayError::Untimed);
            }
            trajectory
                .advance_by(0, sample_rate, at - cursor)
                .map_err(|error| PlayError::Internal(error.to_string()))?;
            cursor = at;
            project_change(&mut trajectory, &change, sample_rate)?;
        }
        let offset = trajectory
            .first_at_or_after(0, sample_rate, plan.endpoint, None)
            .map_err(|error| PlayError::Internal(error.to_string()))?
            .ok_or(PlayError::Untimed)?;
        cursor.checked_add(offset).ok_or(PlayError::Untimed)
    }

    fn trajectory_at(
        &self,
        mark: SlotMark,
        span: SourceSpan,
        offset: u64,
        sample_rate: NonZeroU32,
        commands: &LaneChanges<'_>,
    ) -> Result<SourceTrajectory, PlayError> {
        let mut trajectory = SourceTrajectory::new(self.segment.speed);
        let mut cursor = 0;
        for (at, _, change) in commands.iter().filter(|(at, _, _)| *at <= mark.lane.frame) {
            trajectory
                .advance_by(0, sample_rate, at - cursor)
                .map_err(|error| PlayError::Internal(error.to_string()))?;
            cursor = at;
            project_change(&mut trajectory, &change, sample_rate)?;
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
        end: u64,
    ) -> Result<Option<SessionFrame>, PlayError> {
        mark.session_at(LaneFrame {
            segment: self.segment.id,
            frame: end,
        })
        .map(Some)
        .ok_or(PlayError::Untimed)
    }
}

struct LaneChanges<'a> {
    operations: &'a [LaneOperation],
    current: HashSet<Seq>,
    segment: SegmentId,
    declick: u64,
}

impl<'a> LaneChanges<'a> {
    fn indexed(
        &self,
    ) -> impl DoubleEndedIterator<Item = (u64, Seq, usize, PlannedChange<'a>)> + Clone + '_ {
        let segment = self.segment;
        let declick = self.declick;
        self.operations.iter().filter_map(move |operation| {
            if !operation.is_projected(&self.current) {
                return None;
            }
            let frame = operation.frame_for(segment)?;
            let (at, change) = match &operation.command {
                LaneCommand::SetSpeed(curve) | LaneCommand::Segment { speed: curve, .. } => {
                    (frame, PlannedChange::Speed(curve))
                }
                LaneCommand::Jump { to } => (frame + declick, PlannedChange::Jump(*to)),
                _ => return None,
            };
            Some((at, operation.seq, operation.ordinal, change))
        })
    }

    fn iter(&self) -> impl Iterator<Item = (u64, Seq, PlannedChange<'a>)> + '_ {
        let mut speeds = self
            .indexed()
            .filter(|(_, _, _, change)| matches!(change, PlannedChange::Speed(_)))
            .peekable();
        let mut pending = self
            .indexed()
            .filter(|(_, _, _, change)| matches!(change, PlannedChange::Jump(_)));
        let mut following = pending.clone().peekable();
        let declick = self.declick;
        let mut jumps = std::iter::from_fn(move || {
            loop {
                let current = pending.next()?;
                while following
                    .peek()
                    .is_some_and(|next| (next.0, next.1) <= (current.0, current.1))
                {
                    following.next();
                }
                if following
                    .peek()
                    .is_none_or(|next| next.0 - declick >= current.0)
                {
                    return Some(current);
                }
            }
        })
        .peekable();
        std::iter::from_fn(move || {
            let speed_first = match (speeds.peek(), jumps.peek()) {
                (Some(speed), Some(jump)) => {
                    (speed.0, speed.1, speed.2) <= (jump.0, jump.1, jump.2)
                }
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => return None,
            };
            let (at, seq, _, change) = if speed_first {
                speeds.next()?
            } else {
                jumps.next()?
            };
            Some((at, seq, change))
        })
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
