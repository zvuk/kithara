use kithara_audio::TrackFailureKind;
use kithara_command::{Batch, Outcome, Rejection, Seq, When};
use kithara_config::{ConfigOwner, LiveConfig};
use kithara_render::{
    LaneApplied, LaneCommand, LaneFrame, LaneProtocol, LaneTarget, bridge::PlaybackFault,
};
use kithara_signal::SessionFrame;
use kithara_sync::SpeedCurve;
use tracing::warn;

use super::{
    super::{
        outbox::{Outbox, Settled, rejection},
        settings::{TrackSettings, TrackSettingsChange, TrackSettingsExec},
    },
    core::PlayerImpl,
    geometry::check_curve,
    state::{Configuring, LaneOperation, TrackStatus},
};
use crate::PlayError;

impl PlayerImpl {
    pub(super) fn send_lane(
        &mut self,
        command: LaneCommand,
        when: When<LaneFrame>,
    ) -> Result<Seq, PlayError> {
        self.send_lane_batch(vec![command], when)
    }

    pub(super) fn send_lane_batch(
        &mut self,
        commands: Vec<LaneCommand>,
        when: When<LaneFrame>,
    ) -> Result<Seq, PlayError> {
        self.submit_lane_batch(
            Batch {
                basis: Vec::new(),
                commands,
            },
            when,
        )
    }

    pub(super) fn send_speed_programme(
        &mut self,
        commands: Vec<LaneCommand>,
        when: When<LaneFrame>,
    ) -> Result<Seq, PlayError> {
        let lane = self.lane.sender().ok_or(PlayError::NotReady)?;
        let basis = vec![(LaneTarget::Speed, lane.basis(LaneTarget::Speed, when))];
        self.submit_lane_batch(Batch { basis, commands }, when)
    }

    fn submit_lane_batch(
        &mut self,
        batch: Batch<LaneProtocol>,
        when: When<LaneFrame>,
    ) -> Result<Seq, PlayError> {
        if self.adoptions.cancelling() {
            return Err(PlayError::NotReady);
        }
        let lane = self.lane.sender_mut().ok_or(PlayError::NotReady)?;
        let (seq, Batch { basis, commands }) = Outbox::lane(lane, when, batch)?;
        self.lane_commands.submit(
            self.segment.id,
            commands.into_iter().enumerate().map(|(ordinal, command)| {
                tracing::debug!(
                    item = u64::from(self.item),
                    seq = seq.get(),
                    current_segment = self.segment.id.get(),
                    requested_when = ?when,
                    paused_resume = ?self.resume,
                    mark = ?self.mark,
                    command = ?command,
                    "lane command submitted"
                );
                #[cfg(feature = "usdt")]
                {
                    let target_speed = match &command {
                        LaneCommand::SetSpeed(
                            SpeedCurve::Constant(speed) | SpeedCurve::Ramp { to: speed, .. },
                        ) => Some(*speed),
                        LaneCommand::SetSpeed(SpeedCurve::Steps(steps)) => {
                            steps.last().map(|(_, speed)| *speed)
                        }
                        _ => None,
                    };
                    if let Some(speed) = target_speed
                        && let Some(mark) = self.mark
                    {
                        kithara_test_macros::probe_event!(
                            rate_requested,
                            request_revision = seq.get(),
                            target_rate_bits = speed.to_bits(),
                            session_frame = i64::from(mark.session)
                        );
                    }
                }
                LaneOperation {
                    seq,
                    basis: basis.clone(),
                    ordinal,
                    when,
                    segment: self.segment.id,
                    command,
                    applied: false,
                }
            }),
        );
        Ok(seq)
    }

    pub(super) fn set_speed(
        &mut self,
        mut speed: SpeedCurve,
        at: When<SessionFrame>,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        if let SpeedCurve::Constant(speed) = speed {
            return self.configure(TrackSettingsChange::Speed(speed), at, out);
        }
        let final_speed = check_curve(&mut speed)?;
        self.speed_admission(at, out)?;
        let when = self.lane_when(at, out)?;
        self.execute_speed(speed, final_speed, when, out)
    }

    fn accept_ready(&mut self, seq: Seq, data: &LaneApplied) {
        self.engine_latency = data.engine_latency;
        if let Some(ready) = data.ready
            && !matches!(self.status, TrackStatus::Failed { .. })
        {
            self.segment.report_ready(ready);
            if ready == self.segment.id
                && let Some(basis) = data.source_basis
            {
                self.source_basis = Some(basis);
                kithara_test_macros::probe_event!(
                    lane_source_ready,
                    item = u64::from(self.item),
                    segment = ready.get(),
                    lane_seq = seq.get(),
                    source_revision = basis.source_revision,
                    source_first_frame = basis.frame_offset
                );
            }
        }
    }

    fn apply_lane(&mut self, seq: Seq, at: LaneFrame, data: &LaneApplied) {
        self.segment.answer_request(seq, Ok(()));
        let operations = self.lane_commands.applied(seq, at, self.segment.id);
        for (requested_when, operation) in operations {
            tracing::debug!(
                item = u64::from(self.item),
                seq = seq.get(),
                actual_apply_segment = at.segment.get(),
                actual_apply_frame = at.frame,
                current_segment = self.segment.id.get(),
                requested_when = ?requested_when,
                paused_resume = ?self.resume,
                mark = ?self.mark,
                ready_segment = ?data.ready,
                command = ?operation.command,
                "lane command settled"
            );
            if let LaneCommand::Segment { id, from, .. } = operation.command
                && data.ready == Some(id)
                && id == self.segment.id
                && !matches!(self.status, TrackStatus::Playing { .. })
                && let Some(origin) = self.extent.media_origin
            {
                self.position = from.saturating_sub(origin);
                if matches!(self.status, TrackStatus::Paused { .. }) {
                    self.status = TrackStatus::Paused { at: self.position };
                }
            }
        }
        self.accept_ready(seq, data);
    }

    pub(super) fn settle_lane(&mut self) {
        let accepting = self.lane.sender().is_some();
        while let Some(receipt) = self.lane.receipt() {
            if accepting {
                self.settings.settle(&receipt);
            }
            let seq = receipt.seq();
            let settled = match receipt.outcome() {
                Outcome::Applied { at, data } => {
                    if accepting {
                        self.apply_lane(seq, *at, data);
                    }
                    Settled::Applied { seq, at: *at }
                }
                Outcome::Rejected(reason) => {
                    if accepting {
                        self.segment.answer_request(seq, Err(*reason));
                        self.lane_commands.retain(|operation| operation.seq != seq);
                    }
                    warn!(?reason, "lane command was rejected");
                    Settled::Rejected {
                        seq,
                        reason: rejection(reason, |failure| PlayError::Lane(*failure)),
                    }
                }
            };
            self.lane_commands.receipts.push_back(settled);
        }
        if !accepting {
            return;
        }
        let current = self.segment.id;
        let adoptions = &self.adoptions;
        self.lane_commands.retain(|operation| {
            if !operation.applied {
                return true;
            }
            let live = |segment| {
                segment == current || adoptions.inflight.iter().any(|adoption| {
                    adoption.segment == segment || adoption.parked.is_some_and(|(parked, _)| parked == segment)
                })
            };
            live(operation.segment)
                || matches!(operation.when, When::At(at) if live(at.segment))
                || matches!(operation.command, LaneCommand::Segment { id, .. } | LaneCommand::SetHostRate { id, .. } if live(id))
        });
        self.reserved_ready();
    }

    pub(super) fn configuring(
        &self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_>,
    ) -> Result<(Configuring, Option<usize>), PlayError> {
        if self.adoptions.cancelling() {
            return Err(PlayError::NotReady);
        }
        if self.lane.sender().is_none() {
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
        if self.unchanged(change, when) {
            return Ok((Configuring::Unchanged, None));
        }
        if matches!(change, TrackSettingsChange::Speed(_)) {
            let cost = self.speed_admission(at, out)?;
            return Ok((Configuring::Send(when, change), cost));
        }
        if self.lane.available() == 0 {
            return Err(PlayError::Full("lane"));
        }
        Ok((Configuring::Send(when, change), Some(0)))
    }

    fn unchanged(&self, change: TrackSettingsChange, when: When<LaneFrame>) -> bool {
        match change {
            TrackSettingsChange::Speed(speed) => {
                matches!(when, When::Next) && self.unchanged_speed(speed)
            }
            TrackSettingsChange::Pitch(pitch) => self.settings.projected().pitch() == pitch,
        }
    }

    fn unchanged_speed(&self, speed: f32) -> bool {
        speed == self.settings.config().speed()
            && self
                .loading
                .as_ref()
                .is_none_or(|loading| loading.speed.is_none())
            && !self.lane_commands.operations().iter().any(|operation| {
                !operation.applied
                    && match operation.command {
                        LaneCommand::SetSpeed(_) => operation.segment == self.segment.id,
                        LaneCommand::Segment { id, .. } => id == self.segment.id,
                        _ => false,
                    }
            })
            && self
                .lane_commands
                .operations()
                .iter()
                .filter(|operation| {
                    operation.applied
                        && match operation.command {
                            LaneCommand::SetSpeed(_) => operation.segment == self.segment.id,
                            LaneCommand::Segment { id, .. } => id == self.segment.id,
                            _ => false,
                        }
                })
                .max_by_key(|operation| (operation.when, operation.seq, operation.ordinal))
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
        out: &Outbox<'_>,
    ) -> Result<Option<usize>, PlayError> {
        if self.lane.sender().is_none() {
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
        if self.lane.available() == 0 {
            return Err(PlayError::Full("lane"));
        }
        Ok(Some(0))
    }

    pub(super) fn execute_speed(
        &mut self,
        speed: SpeedCurve,
        target: f32,
        when: When<LaneFrame>,
        out: &mut Outbox<'_>,
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
        let seq = self.send_speed_programme(vec![LaneCommand::SetSpeed(speed)], when)?;
        self.settings
            .track(seq, when, TrackSettingsChange::Speed(target));
        Ok(Some(seq))
    }

    pub(super) fn configure(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &mut Outbox<'_>,
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
        match self.segment.request_verdict() {
            Some(Err(Rejection::Refused(failure))) => Some(failure),
            _ => None,
        }
    }
}

impl<'a> TrackSettingsExec<Outbox<'a>> for PlayerImpl {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    fn exec_live(
        &mut self,
        change: TrackSettingsChange,
        at: Self::At,
        out: &mut Outbox<'a>,
    ) -> Self::Output {
        self.configure(change, at, out)
    }

    fn exec_speed(&mut self, speed: f32, at: Self::At, out: &mut Outbox<'a>) -> Self::Output {
        self.configure(TrackSettingsChange::Speed(speed), at, out)
    }
}
