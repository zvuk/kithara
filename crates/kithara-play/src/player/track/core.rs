use std::{cell::RefCell, collections::VecDeque, marker::PhantomData};

use kithara_abr::AbrHandle;
use kithara_command::{Live, Sender, Seq, Target, When};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_render::{
    LaneCommand, LaneFrame, LaneId, LaneProtocol, ServiceClass,
    bridge::{DeckPart, Fade, Slot, SlotMark},
};
use kithara_signal::{AudioChunkInfo, FrameCount, SegmentId, SessionFrame};
use kithara_sync::SpeedCurve;
use num_traits::ToPrimitive;

use super::{
    super::{
        outbox::Outbox,
        settings::{PlayerConfig, TrackSettings, TrackSettingsChange},
    },
    geometry::*,
    state::*,
};
use crate::PlayError;

/// The sole segment issuer and command producer for one track.
pub struct PlayerImpl<S> {
    pub(super) item: TrackId,
    pub(super) slot: Option<Slot>,
    pub(super) class: ServiceClass,
    pub(super) lane: Option<Sender<LaneProtocol>>,
    pub(super) lane_id: Option<LaneId>,
    pub(super) settings: Live<TrackSettings, LaneProtocol>,
    pub(super) status: TrackStatus,
    pub(super) loading: Option<Loading>,
    pub(super) attaching: Option<Attaching>,
    pub(super) segment: SegmentId,
    pub(super) ready: Option<SegmentId>,
    pub(super) issued_segment: SegmentId,
    pub(super) adopting: Vec<Adoption>,
    pub(super) newest_epoch_pending: Option<Seq>,
    pub(super) lane_commands: Vec<LaneOperation>,
    pub(super) playback_commands: Vec<PlaybackOperation>,
    pub(super) speed_answers: VecDeque<SpeedAnswer>,
    pub(super) segment_speed: f32,
    pub(super) play: Option<When<SessionFrame>>,
    pub(super) repeat: bool,
    pub(super) releasing: bool,
    pub(super) resume: Option<SlotMark>,
    pub(super) mark: Option<SlotMark>,
    pub(super) attach_at: Option<SessionFrame>,
    pub(super) ring_depth: FrameCount,
    pub(super) engine_latency: FrameCount,
    pub(super) declick: FrameCount,
    pub(super) position: Position,
    pub(super) segment_origin: Option<Position>,
    pub(super) source_basis: Option<AudioChunkInfo>,
    pub(super) observed_source: Option<AudioChunkInfo>,
    pub(super) projected_end: RefCell<Option<ProjectedEnd>>,
    pub(super) duration: Option<Duration>,
    pub(super) visible_origin: Option<Duration>,
    pub(super) media_origin: Option<Duration>,
    pub(super) abr: Option<AbrHandle>,
    pub(super) metadata: TrackMetadata,
    pub(super) release: Option<Seq>,
    pub(super) marker: PhantomData<fn() -> S>,
}

impl<S> PlayerImpl<S> {
    /// Builds an idle track for its assigned item and mixer slot.
    ///
    /// # Errors
    /// Returns a refusal for invalid track settings.
    pub fn new(config: PlayerConfig) -> Result<Self, PlayError> {
        Ok(Self {
            item: config.item,
            slot: config.slot,
            class: ServiceClass::default(),
            lane: None,
            lane_id: None,
            settings: Live::new(config.settings)?,
            status: TrackStatus::Idle,
            loading: None,
            attaching: None,
            segment: SegmentId::FIRST,
            ready: None,
            issued_segment: SegmentId::FIRST,
            adopting: Vec::new(),
            newest_epoch_pending: None,
            lane_commands: Vec::new(),
            playback_commands: Vec::new(),
            speed_answers: VecDeque::new(),
            segment_speed: config.settings.speed(),
            play: None,
            repeat: false,
            releasing: false,
            resume: None,
            mark: None,
            attach_at: None,
            ring_depth: FrameCount::new(0),
            engine_latency: FrameCount::new(0),
            declick: FrameCount::new(0),
            position: Position::ZERO,
            segment_origin: None,
            source_basis: None,
            observed_source: None,
            projected_end: RefCell::new(None),
            duration: None,
            visible_origin: None,
            media_origin: None,
            abr: None,
            metadata: TrackMetadata::default(),
            release: None,
            marker: PhantomData,
        })
    }

    #[must_use]
    pub fn status(&self) -> TrackStatus {
        self.status
    }

    pub(super) fn seat(&self) -> Result<Slot, PlayError> {
        self.slot.ok_or(PlayError::NoActiveSlot)
    }

    #[must_use]
    pub fn attached(&self) -> bool {
        self.attaching.is_some() || self.attach_at.is_some()
    }

    /// Content position of a raw source time. A raw time read from the
    /// source keeps the observed source's frame grid across the subtraction.
    pub(super) fn content_position(&self, raw: Position) -> Option<Position> {
        let origin = self.media_origin?;
        Some(self.observed_source.map_or_else(
            || raw.saturating_sub(origin),
            |source| source.spec.elapsed_since(origin, raw),
        ))
    }

    pub(super) fn content_mark_position(&self, mark: SlotMark) -> Option<Position> {
        if mark.lane.frame == 0 {
            self.media_origin
                .map(|origin| mark.position.saturating_sub(origin))
        } else {
            self.content_position(mark.position)
        }
    }

    pub(super) fn raw_position(&self, position: Position) -> Result<Position, PlayError> {
        self.media_origin
            .and_then(|origin| origin.checked_add(position))
            .ok_or(PlayError::Untimed)
    }

    pub(super) fn speed_position(&self) -> Result<Position, PlayError> {
        if let Some(mark) = self
            .resume
            .or(self.mark)
            .filter(|mark| mark.lane.segment == self.segment)
        {
            return Ok(mark.position);
        }
        self.segment_origin.ok_or(PlayError::Untimed)
    }

    pub(super) fn observe(&mut self, out: &Outbox<'_, S>) {
        if self.loading.is_some() {
            return;
        }
        if let Some(seat) = self.slot
            && let Some(pass) = out.pass()
            && let Some(slot) = pass.deck.slots.get(seat.index())
        {
            self.mark = slot.mark;
            if let Some(source) = slot.source.filter(|info| info.segment == self.segment)
                && self.attach_at.is_some()
                && !matches!(self.status, TrackStatus::Released)
            {
                if self.segment_origin.is_none() {
                    self.segment_origin = Some(source.timestamp);
                }
                self.observed_source = Some(source);
                self.visible_origin = source.visible_origin;
                self.media_origin = source.media_origin;
                self.duration = source.media_end;
            }
            if let Some(mark) = slot.mark.filter(|mark| mark.lane.segment == self.segment)
                && slot.position.is_finite()
                && slot.position >= 0.0
                && let Some(position) = self.content_mark_position(mark)
            {
                self.position = position;
            }
        }
    }

    pub(super) fn planned_changes(
        &self,
        segment: SegmentId,
        future: bool,
    ) -> Result<Vec<(u64, Seq, PlannedChange<'_>)>, PlayError> {
        let mut commands: Vec<(u64, Seq, PlannedChange<'_>)> = Vec::new();
        for operation in &self.lane_commands {
            if operation.applied() == Some(false) {
                continue;
            }
            let frame = match &operation.command {
                LaneCommand::Segment { id, .. } if *id == segment => 0,
                _ => match operation.when {
                    When::At(frame) if frame.segment == segment => frame.frame,
                    When::Next | When::Deferred
                        if operation.segment == segment
                            && matches!(
                                operation.command,
                                LaneCommand::SetSpeed(_) | LaneCommand::Jump { .. }
                            ) =>
                    {
                        if future {
                            return Err(PlayError::NotReady);
                        }
                        continue;
                    }
                    When::At(_) | When::Next | When::Deferred => continue,
                },
            };
            match &operation.command {
                LaneCommand::SetSpeed(curve) | LaneCommand::Segment { speed: curve, .. } => {
                    commands.push((frame, operation.seq, PlannedChange::Speed(curve)));
                }
                LaneCommand::Jump { to } => {
                    let landing = frame
                        .checked_add(self.declick.get() as u64)
                        .ok_or(PlayError::Untimed)?;
                    let superseded = self.lane_commands.iter().any(|next| next.seq != operation.seq && next.applied() != Some(false)
                        && matches!(next.command, LaneCommand::Jump { .. })
                        && matches!(next.when, When::At(at) if at.segment == segment && (at.frame > frame || at.frame == frame && next.seq > operation.seq) && at.frame < landing));
                    if !superseded {
                        commands.push((landing, operation.seq, PlannedChange::Jump(*to)));
                    }
                }
                _ => {}
            }
        }
        commands.sort_by_key(|&(frame, seq, _)| (frame, seq));
        Ok(commands)
    }

    pub(super) fn consumed_speed(&self) -> Result<f32, PlayError> {
        let frame = match self
            .resume
            .or(self.mark)
            .filter(|mark| mark.lane.segment == self.segment)
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
        let commands = self.planned_changes(self.segment, false)?;
        let base = SpeedCurve::Constant(self.segment_speed);
        let mut curve = &base;
        let mut origin = f64::from(self.segment_speed);
        let mut start = 0;
        for (at, _, change) in commands.into_iter().filter(|(at, _, _)| *at <= frame) {
            if let PlannedChange::Speed(next) = change {
                origin = curve_speed(curve, origin, at.saturating_sub(start))?;
                curve = next;
                start = at;
            }
        }
        curve_speed(curve, origin, frame.saturating_sub(start))?
            .to_f32()
            .ok_or(PlayError::Untimed)
    }

    pub(super) fn check_when(
        &self,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        if let When::At(frame) = at {
            let pass = out.pass().ok_or(PlayError::Untimed)?;
            let origin = self
                .attach_at
                .map_or(pass.now, |attach| attach.max(pass.now));
            if frame < origin + pass.delivery {
                return Err(PlayError::Late);
            }
            if self.attach_at.is_none() {
                return Err(PlayError::Untimed);
            }
        }
        Ok(())
    }

    pub(super) fn lane_when(
        &self,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<When<LaneFrame>, PlayError> {
        match at {
            When::Next => Ok(When::Next),
            When::Deferred => Err(PlayError::Internal(
                "the lane has no end-marker clock".into(),
            )),
            When::At(frame) => {
                self.check_when(at, out)?;
                let pass = out.pass().ok_or(PlayError::Untimed)?;
                if frame < pass.earliest() + self.ring_depth + self.engine_latency {
                    return Err(PlayError::Late);
                }
                let mark = self.mark.ok_or(PlayError::Untimed)?;
                if mark.lane.segment != self.segment
                    || !matches!(self.status, TrackStatus::Playing { .. })
                {
                    return Err(PlayError::Untimed);
                }
                mark.lane_at(frame).map(When::At).ok_or(PlayError::Untimed)
            }
        }
    }

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
        if self.adopting.iter().any(|adoption| adoption.cancelled) {
            return Err(PlayError::NotReady);
        }
        let session = match when {
            When::At(frame) => self.mark.and_then(|mark| session_at(mark, frame)),
            When::Next | When::Deferred => None,
        };
        let lane = self.lane.as_mut().ok_or(PlayError::NotReady)?;
        let seq = Outbox::<S>::lane(lane, when, commands.clone())?;
        self.lane_commands
            .extend(commands.into_iter().map(|command| {
                tracing::debug!(
                    item = u64::from(self.item),
                    seq = seq.get(),
                    current_segment = self.segment.get(),
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
                    when,
                    segment: self.segment,
                    session,
                    command,
                    verdict: None,
                }
            }));
        Ok(seq)
    }

    pub(super) fn jump_when(
        &self,
        at: SessionFrame,
        out: &Outbox<'_, S>,
    ) -> Result<When<LaneFrame>, PlayError> {
        let start = i64::from(at)
            .checked_sub(
                i64::try_from(self.declick.get())
                    .map_err(|error| PlayError::Internal(error.to_string()))?,
            )
            .map(SessionFrame::new)
            .ok_or(PlayError::Late)?;
        self.lane_when(When::At(start), out)
    }

    pub(super) fn pause(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_when(at, out)?;
        self.play = None;
        if let Some(attaching) = self.attaching.as_mut() {
            attaching.play = None;
        }
        if !self.attached() {
            return Ok(None);
        }
        self.send_playback(
            at,
            vec![DeckPart::Stop {
                slot: self.seat()?,
                fade: Fade::Declick,
            }],
            out,
        )
    }

    pub(super) fn set_speed(
        &mut self,
        mut speed: SpeedCurve,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if let SpeedCurve::Constant(speed) = speed {
            return self.configure(TrackSettingsChange::Speed(speed), at, out);
        }
        let final_speed = check_curve(&mut speed)?;
        self.speed_admission(at, out)?;
        let when = self.lane_when(at, out)?;
        self.execute_speed(speed, final_speed, when, out)
    }
}
