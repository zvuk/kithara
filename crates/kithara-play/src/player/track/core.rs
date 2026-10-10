use std::{collections::VecDeque, marker::PhantomData};

use kithara_abr::AbrHandle;
use kithara_command::{Batch, Live, Sender, Seq, Target, When};
use kithara_config::LiveConfig;
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_render::{
    LaneCommand, LaneFrame, LaneId, LaneProtocol, ServiceClass,
    bridge::{DeckPart, Fade, Slot, SlotMark},
};
use kithara_signal::{FrameCount, SegmentId, SessionFrame};
use kithara_warp::SpeedCurve;

use super::{
    super::{
        outbox::Outbox,
        settings::{PlayerConfig, TrackSettings, TrackSettingsChange, check_speed},
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
    pub(super) adopt_retry: Option<Seq>,
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
    pub(super) duration: Option<Duration>,
    pub(super) abr: Option<AbrHandle>,
    pub(super) metadata: TrackMetadata,
    pub(super) release: Option<Seq>,
    pub(super) marker: PhantomData<fn() -> S>,
}

impl<S> PlayerImpl<S> {
    /// Builds an idle track for its assigned item and mixer slot.
    pub(crate) fn new(config: PlayerConfig) -> Result<Self, PlayError> {
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
            adopt_retry: None,
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
            duration: None,
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
        self.attaching.is_some()
            || self.loading.is_none()
                && !matches!(
                    self.status,
                    TrackStatus::Idle | TrackStatus::Loading | TrackStatus::Released
                )
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
            if slot.position.is_finite() && slot.position >= 0.0 {
                self.position = Position::from_secs_f64(slot.position);
            }
            if slot.duration.is_finite() && slot.duration > 0.0 {
                self.duration = Some(Duration::from_secs_f64(slot.duration));
            }
        }
    }

    pub(super) fn planned_changes(
        &self,
        mark: SlotMark,
    ) -> Result<Vec<(u64, Seq, PlannedChange<'_>)>, PlayError> {
        let mut commands: Vec<(u64, Seq, PlannedChange<'_>)> = Vec::new();
        for operation in &self.lane_commands {
            if operation.applied == Some(false) {
                continue;
            }
            let frame = match operation.when {
                When::At(frame) if frame.segment == mark.lane.segment => frame.frame,
                When::Next | When::Deferred
                    if operation.segment == mark.lane.segment
                        && matches!(
                            operation.command,
                            LaneCommand::SetSpeed(_) | LaneCommand::Jump { .. }
                        ) =>
                {
                    return Err(PlayError::NotReady);
                }
                When::At(_) | When::Next | When::Deferred => continue,
            };
            match &operation.command {
                LaneCommand::SetSpeed(curve) => {
                    commands.push((frame, operation.seq, PlannedChange::Speed(curve)));
                }
                LaneCommand::Jump { to } => {
                    let landing = frame
                        .checked_add(self.declick.get() as u64)
                        .ok_or(PlayError::Untimed)?;
                    let superseded = self.lane_commands.iter().any(|next| next.seq != operation.seq && next.applied != Some(false)
                        && matches!(next.command, LaneCommand::Jump { .. })
                        && matches!(next.when, When::At(at) if at.segment == mark.lane.segment && (at.frame > frame || at.frame == frame && next.seq > operation.seq) && at.frame < landing));
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
        if self.adopting.iter().any(|adoption| adoption.cancelled) {
            return Err(PlayError::NotReady);
        }
        let session = match when {
            When::At(frame) => self.mark.and_then(|mark| session_at(mark, frame)),
            When::Next | When::Deferred => None,
        };
        let lane = self.lane.as_mut().ok_or(PlayError::NotReady)?;
        let seq = lane
            .send(
                when,
                Batch {
                    basis: Vec::new(),
                    commands: vec![command.clone()],
                },
            )
            .map_err(|error| lane_refusal(&error))?;
        self.lane_commands.push(LaneOperation {
            seq,
            when,
            segment: self.segment,
            session,
            command,
            applied: None,
        });
        Ok(seq)
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
        if matches!(at, When::Next)
            && matches!(
                self.status,
                TrackStatus::Loaded | TrackStatus::Paused { .. }
            )
            && self.playback_commands.is_empty()
        {
            self.status = TrackStatus::Paused { at: self.position };
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
        let final_speed = match &mut speed {
            SpeedCurve::Ramp { to, .. } => {
                *to = check_speed(*to)?;
                *to
            }
            SpeedCurve::Steps(steps) => {
                let Some(&(_, last)) = steps.last() else {
                    return Err(PlayError::Internal(
                        "an empty speed curve has no target".into(),
                    ));
                };
                let mut previous = None;
                for (frame, value) in Arc::make_mut(steps) {
                    *value = check_speed(*value)?;
                    if previous.is_some_and(|prior| *frame <= prior) {
                        return Err(PlayError::Internal(
                            "speed steps must increase on the output axis".into(),
                        ));
                    }
                    previous = Some(*frame);
                }
                check_speed(last)?
            }
            _ => return Err(PlayError::Internal("unsupported speed curve".into())),
        };
        let change = TrackSettings::check(TrackSettingsChange::Speed(final_speed))?;
        let when = self.lane_when(at, out)?;
        let seq = self.send_lane(LaneCommand::SetSpeed(speed), when)?;
        self.settings.track(seq, when, change);
        Ok(Some(seq))
    }
}
