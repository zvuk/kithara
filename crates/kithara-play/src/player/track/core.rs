use kithara_abr::AbrHandle;
use kithara_command::{Live, Seq, Target, When};
use kithara_config::ConfigOwner;
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::sync::Arc;
use kithara_render::{
    LaneFrame, LaneProtocol, ServiceClass,
    bridge::{DeckPart, Fade, Slot, SlotMark, TrackClock},
};
use kithara_signal::{AudioChunkInfo, FrameCount, SegmentId, SessionFrame};
use kithara_stream::PlayheadRead;

use super::{
    super::{
        outbox::Outbox,
        settings::{PlayerConfig, TrackSettings},
    },
    seat::Seat,
    segment::{Adoptions, Segment},
    state::{Extent, Lane, LaneCommands, Loading, PlaybackOperation, Position, TrackStatus},
};
use crate::{PlayError, resource::LaneGeometry};

/// The sole segment issuer and command producer for one track.
pub struct PlayerImpl {
    pub(super) item: TrackId,
    pub(super) seat: Seat,
    pub(super) class: ServiceClass,
    pub(super) lane: Lane,
    pub(super) settings: Live<TrackSettings, LaneProtocol>,
    pub(super) output: kithara_render::bridge::SlotOutputSnapshot,
    pub(super) status: TrackStatus,
    pub(super) loading: Option<Loading>,
    pub(super) segment: Segment,
    pub(super) issued_segment: SegmentId,
    pub(super) adoptions: Adoptions,
    pub(super) lane_commands: LaneCommands,
    pub(super) playback_commands: Vec<PlaybackOperation>,
    pub(super) play: Option<When<SessionFrame>>,
    pub(super) repeat: bool,
    /// The clock the track keeps through a dropout.
    pub(super) clock: TrackClock,
    /// The clock its seated RT track was last sent.
    pub(super) seated_clock: TrackClock,
    pub(super) resume: Option<SlotMark>,
    pub(super) mark: Option<SlotMark>,
    pub(super) geometry: LaneGeometry,
    pub(super) engine_latency: FrameCount,
    pub(super) position: Position,
    pub(super) source_basis: Option<AudioChunkInfo>,
    pub(super) observed_source: Option<AudioChunkInfo>,
    pub(super) extent: Extent,
    pub(super) playhead: Option<Arc<dyn PlayheadRead>>,
    pub(super) abr: Option<AbrHandle>,
    pub(super) metadata: TrackMetadata,
}

impl PlayerImpl {
    /// Builds an idle track for its assigned item and mixer slot.
    ///
    /// # Errors
    /// Returns a refusal for invalid track settings.
    pub fn new(config: PlayerConfig) -> Result<Self, PlayError> {
        Ok(Self {
            item: config.item,
            seat: config.slot.map_or(Seat::Unseated, Seat::Named),
            class: ServiceClass::default(),
            lane: Lane::None,
            settings: Live::new(config.settings)?,
            output: kithara_render::bridge::SlotOutputSnapshot::default(),
            status: TrackStatus::Idle,
            loading: None,
            segment: Segment::issued(SegmentId::FIRST, None, config.settings.speed()),
            issued_segment: SegmentId::FIRST,
            adoptions: Adoptions::default(),
            lane_commands: LaneCommands::default(),
            playback_commands: Vec::new(),
            play: None,
            repeat: false,
            clock: TrackClock::Media,
            seated_clock: TrackClock::Media,
            resume: None,
            mark: None,
            geometry: LaneGeometry::default(),
            engine_latency: FrameCount::new(0),
            position: Position::ZERO,
            source_basis: None,
            observed_source: None,
            extent: Extent::default(),
            playhead: None,
            abr: None,
            metadata: TrackMetadata::default(),
        })
    }

    #[must_use]
    pub fn status(&self) -> TrackStatus {
        self.status
    }

    pub(super) fn close_lane_admission(&mut self) {
        if self.lane.sender().is_none() {
            return;
        }
        self.settle_lane();
        self.settings.abandon();
        self.lane.retire();
        self.playhead = None;
        self.lane_commands.clear();
        self.segment.ready = false;
        self.source_basis = None;
        self.observed_source = None;
        self.mark = None;
        self.resume = None;
    }

    pub(super) fn finish_lane(&mut self) {
        self.settle_lane();
        self.lane = Lane::None;
    }

    pub(super) fn select_segment(&mut self, segment: Segment) {
        self.segment = segment;
        self.lane_commands.select(self.segment.id);
    }

    pub(super) fn reset_segments(&mut self) {
        self.select_segment(Segment::issued(
            SegmentId::FIRST,
            None,
            self.settings.config().speed(),
        ));
        self.issued_segment = SegmentId::FIRST;
    }

    pub(super) fn seat(&self) -> Result<Slot, PlayError> {
        self.seat.slot().ok_or(PlayError::NoActiveSlot)
    }

    #[must_use]
    pub fn attached(&self) -> bool {
        matches!(self.seat, Seat::Attaching { .. } | Seat::Attached { .. })
    }

    /// Content position of a raw source time. A raw time read from the
    /// source keeps the observed source's frame grid across the subtraction.
    pub(super) fn content_position(&self, raw: Position) -> Option<Position> {
        Some(self.content_position_from(self.extent.media_origin?, raw))
    }

    /// Content position of a raw source time measured from `origin`.
    pub(super) fn content_position_from(&self, origin: Position, raw: Position) -> Position {
        self.observed_source.map_or_else(
            || raw.saturating_sub(origin),
            |source| source.spec.elapsed_since(origin, raw),
        )
    }

    pub(super) fn content_mark_position(&self, mark: SlotMark) -> Option<Position> {
        if mark.lane.frame == 0 {
            self.extent
                .media_origin
                .map(|origin| mark.position.saturating_sub(origin))
        } else {
            self.content_position(mark.position)
        }
    }

    pub(super) fn raw_position(&self, position: Position) -> Result<Position, PlayError> {
        self.extent
            .media_origin
            .and_then(|origin| origin.checked_add(position))
            .ok_or(PlayError::Untimed)
    }

    pub(super) fn speed_position(&self) -> Result<Position, PlayError> {
        if let Some(mark) = self
            .resume
            .or(self.mark)
            .filter(|mark| mark.lane.segment == self.segment.id)
        {
            return Ok(mark.position);
        }
        self.segment.origin.ok_or(PlayError::Untimed)
    }

    pub(super) fn observe(&mut self, out: &Outbox<'_>) {
        if self.loading.is_some() || self.seat.attached_at().is_none() {
            return;
        }
        if let Some(seat) = self.seat.slot()
            && let Some(pass) = out.pass()
            && let Some(slot) = pass.deck.slots.get(seat.index())
        {
            self.mark = slot.mark;
            if slot.binding.is_some_and(|binding| {
                binding.item() == self.item && binding.segment() == self.segment.id
            }) {
                self.output = slot.output;
            }
            if let Some(source) = slot.source.filter(|info| info.segment == self.segment.id)
                && !matches!(self.status, TrackStatus::Released)
            {
                if self.segment.origin.is_none() {
                    self.segment.origin = Some(source.timestamp);
                }
                self.observed_source = Some(source);
                self.extent = Extent::of(Some(source));
            }
            if let Some(mark) = slot
                .mark
                .filter(|mark| mark.lane.segment == self.segment.id)
                && slot.position.is_finite()
                && slot.position >= 0.0
                && let Some(position) = self.content_mark_position(mark)
            {
                self.position = position;
            }
        }
    }

    pub(super) fn check_when(
        &self,
        at: When<SessionFrame>,
        out: &Outbox<'_>,
    ) -> Result<(), PlayError> {
        out.check_when(at)?;
        if let When::At(_) = at
            && self.seat.attached_at().is_none()
        {
            return Err(PlayError::Untimed);
        }
        Ok(())
    }

    pub(super) fn lane_when(
        &self,
        at: When<SessionFrame>,
        out: &Outbox<'_>,
    ) -> Result<When<LaneFrame>, PlayError> {
        match at {
            When::Next => Ok(When::Next),
            When::Deferred => Err(PlayError::Internal(
                "the lane has no end-marker clock".into(),
            )),
            When::At(frame) => {
                self.check_when(at, out)?;
                let pass = out.pass().ok_or(PlayError::Untimed)?;
                if frame < pass.earliest() + self.geometry.ring_depth + self.engine_latency {
                    return Err(PlayError::Late);
                }
                let mark = self.mark.ok_or(PlayError::Untimed)?;
                if mark.lane.segment != self.segment.id
                    || !matches!(self.status, TrackStatus::Playing { .. })
                {
                    return Err(PlayError::Untimed);
                }
                mark.lane_at(frame).map(When::At).ok_or(PlayError::Untimed)
            }
        }
    }

    pub(super) fn jump_when(
        &self,
        at: SessionFrame,
        out: &Outbox<'_>,
    ) -> Result<When<LaneFrame>, PlayError> {
        let start = i64::from(at)
            .checked_sub(
                i64::try_from(self.geometry.declick.get())
                    .map_err(|error| PlayError::Internal(error.to_string()))?,
            )
            .map(SessionFrame::new)
            .ok_or(PlayError::Late)?;
        self.lane_when(When::At(start), out)
    }

    /// Drops the start this track still owes, with its pending attachment's.
    pub(super) fn withdraw_play(&mut self) {
        self.play = None;
        if let Some(attaching) = self.seat.attaching_mut() {
            attaching.play = None;
        }
    }

    pub(super) fn pause(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_when(at, out)?;
        self.withdraw_play();
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
}
