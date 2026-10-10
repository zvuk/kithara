use std::num::NonZeroU32;

use kithara_abr::AbrHandle;
use kithara_command::{Rejection, Seq, When};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_render::{
    CrossfadeSettings, LaneCommand, LaneFrame,
    bridge::{FadeDir, PlaybackFault, Slot, SlotMark},
};
use kithara_signal::{FrameCount, SegmentId, SessionFrame};
use kithara_warp::SpeedCurve;

use super::super::settings::TrackSettingsChange;
use crate::{OpenedTrack, PlayError, ResourceLoad};

/// A position in a track's media.
pub type Position = Duration;

/// What a track is told to do.
pub enum TrackCommand<S> {
    Load {
        item: ResourceLoad<S>,
        position: Position,
    },
    Play {
        at: When<SessionFrame>,
    },
    Pause {
        at: When<SessionFrame>,
    },
    Seek {
        to: Position,
    },
    /// Places the new position on this session frame, after the lane's declick.
    Jump {
        to: Position,
        at: SessionFrame,
    },
    Configure(TrackSettingsChange, When<SessionFrame>),
    SetSpeed {
        speed: SpeedCurve,
        at: When<SessionFrame>,
    },
    /// Opens a new segment at the output route's rate.
    SetHostRate {
        rate: NonZeroU32,
    },
    Fade {
        at: When<SessionFrame>,
        settings: CrossfadeSettings,
        dir: FadeDir,
    },
    PlayAfter {
        track: Slot,
    },
    /// Supersedes scheduled slot batches without stopping the sounding segment.
    Supersede,
    Release,
    /// Seats a track once: Next attaches its held PCM as soon as it opens;
    /// At(frame) replaces the slot's sounding consumer on that frame.
    Seat {
        slot: Slot,
        at: When<SessionFrame>,
    },
}

/// Where a track stands, as the receipts of its executors left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackStatus {
    Idle,
    Loading,
    Loaded,
    Playing {
        since: SessionFrame,
    },
    Paused {
        at: Position,
    },
    Faded {
        at: SessionFrame,
    },
    Ended {
        at: SessionFrame,
    },
    Failed {
        at: SessionFrame,
        fault: PlaybackFault,
    },
    Released,
}

/// The owner's settled track state and the latest mixer observation.
#[derive(Clone)]
pub struct TrackSnapshot {
    pub item: TrackId,
    /// None while a background load holds the track off the deck.
    pub slot: Option<Slot>,
    pub status: TrackStatus,
    pub speed: f32,
    pub position: Position,
    pub duration: Option<Duration>,
    pub abr: Option<AbrHandle>,
    pub metadata: TrackMetadata,
    pub mark: Option<SlotMark>,
    pub engine_latency: FrameCount,
    pub ring_depth: FrameCount,
    pub lane_room: usize,
    pub pending_lane: bool,
    pub attached: bool,
    pub declick: FrameCount,
}

impl std::fmt::Debug for TrackSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TrackSnapshot")
            .field("item", &self.item)
            .field("slot", &self.slot)
            .field("status", &self.status)
            .field("speed", &self.speed)
            .field("position", &self.position)
            .field("duration", &self.duration)
            .field("abr", &self.abr.is_some())
            .field("metadata", &self.metadata)
            .field("mark", &self.mark)
            .field("engine_latency", &self.engine_latency)
            .field("ring_depth", &self.ring_depth)
            .field("lane_room", &self.lane_room)
            .field("pending_lane", &self.pending_lane)
            .field("attached", &self.attached)
            .field("declick", &self.declick)
            .finish()
    }
}

impl AsRef<Self> for TrackSnapshot {
    fn as_ref(&self) -> &Self {
        self
    }
}

pub(super) struct Loading {
    pub(super) seq: Seq,
    pub(super) opened: Option<OpenedTrack>,
}

pub(super) struct Adoption {
    pub(super) seq: Seq,
    pub(super) caller: Seq,
    pub(super) segment: SegmentId,
    pub(super) parked: Option<(SegmentId, Position)>,
    pub(super) cancelled: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Attaching {
    pub(super) seq: Option<Seq>,
    pub(super) caller: Seq,
    pub(super) at: When<SessionFrame>,
    pub(super) replacement: bool,
    pub(super) play: Option<When<SessionFrame>>,
}

pub(super) struct LaneOperation {
    pub(super) seq: Seq,
    pub(super) when: When<LaneFrame>,
    pub(super) segment: SegmentId,
    pub(super) session: Option<SessionFrame>,
    pub(super) command: LaneCommand,
    pub(super) applied: Option<bool>,
}

pub(super) struct PlaybackOperation {
    pub(super) seq: Option<Seq>,
    pub(super) segment: SegmentId,
    pub(super) basis: Vec<(Slot, Option<Seq>)>,
}

pub(super) type SpeedAnswer = (
    Seq,
    Result<(LaneFrame, Option<SessionFrame>), Rejection<PlayError>>,
);

pub(super) enum Configuring {
    Apply(TrackSettingsChange),
    Unchanged,
    Send(When<LaneFrame>, TrackSettingsChange),
}

pub(super) enum PlannedChange<'a> {
    Speed(&'a SpeedCurve),
    Jump(Position),
}
