use std::num::{NonZeroU32, NonZeroU64, NonZeroU128};

use kithara_abr::AbrHandle;
use kithara_audio::TrackFailureKind;
use kithara_command::{Rejection, Seq, When};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_render::{
    CrossfadeSettings, LaneCommand, LaneFrame,
    bridge::{FadeDir, PlaybackFault, Slot, SlotMark},
};
use kithara_signal::{AudioChunkInfo, FrameCount, SegmentId, SessionFrame};
use kithara_sync::SpeedCurve;

use super::super::settings::TrackSettingsChange;
use crate::{OpenedTrack, PlayError, ResourceLoad};

/// A position in a track's media.
pub type Position = Duration;

/// What a track is told to do.
pub enum TrackCommand<S> {
    EffectProgram {
        lane: kithara_render::LaneId,
        program: kithara_render::bridge::EffectProgram,
    },
    GainProgram(kithara_render::bridge::SlotGainProgram),
    ConfigureEffect {
        lane: kithara_render::LaneId,
        target: kithara_effects::EffectTarget,
        change: kithara_effects::EffectChange,
        at: When<SessionFrame>,
    },
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
    /// Sets speed and lands the jump on `at` as one lane batch.
    Align {
        to: Position,
        speed: SpeedCurve,
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
    pub output: kithara_render::bridge::SlotOutputSnapshot,
    pub effects: Vec<super::effects::EffectSnapshot>,
    pub item: TrackId,
    /// None while a background load holds the track off the deck.
    pub slot: Option<Slot>,
    pub status: TrackStatus,
    pub speed: f32,
    /// Last known media position; an unconsumed segment retains its exact entry mark.
    pub position: Position,
    /// End of the track on the same media clock as its grid and cues.
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
            .field("output", &self.output)
            .field("effects", &self.effects)
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
    pub(super) speed: Option<(SpeedCurve, f32)>,
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
    pub(super) obsolete: bool,
}

pub(super) struct LaneOperation {
    pub(super) seq: Seq,
    pub(super) when: When<LaneFrame>,
    pub(super) segment: SegmentId,
    pub(super) session: Option<SessionFrame>,
    pub(super) command: LaneCommand,
    pub(super) verdict: Option<Result<(), Rejection<TrackFailureKind>>>,
}

impl LaneOperation {
    pub(super) fn changes_time(&self) -> bool {
        !matches!(
            self.command,
            LaneCommand::EffectProgram { .. } | LaneCommand::ConfigureEffect { .. }
        )
    }

    pub(super) fn applied(&self) -> Option<bool> {
        self.verdict.map(|verdict| verdict.is_ok())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackAction {
    Start,
    Stop,
    Detach,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackQuery {
    Admission,
    Receipt,
}

pub(super) struct PlaybackOperation {
    pub(super) seq: Option<Seq>,
    pub(super) segment: SegmentId,
    pub(super) basis: Vec<(Slot, Option<Seq>)>,
    pub(super) action: Option<PlaybackAction>,
    /// A later applied transport already determines this operation's status.
    pub(super) obsolete: bool,
    /// The newest applied envelope descendant of this pending transport.
    pub(super) transparent: Option<Seq>,
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

/// Every input of an end projection except the observation point: within one
/// plan the exact source law reaches its endpoint at the same lane frame
/// wherever the track last observed its source.
#[derive(Clone, PartialEq)]
pub(super) struct EndPlan {
    pub(super) source: (Option<u64>, u64, Option<NonZeroU64>),
    pub(super) basis: AudioChunkInfo,
    pub(super) endpoint: (u128, NonZeroU128),
    pub(super) speed: u32,
    pub(super) sample_rate: NonZeroU32,
    pub(super) commands: Vec<(u64, Seq)>,
}

/// The lane frame where the source reaches its endpoint under one plan.
pub(super) struct ProjectedEnd {
    pub(super) plan: EndPlan,
    pub(super) frame: u64,
}

pub(super) enum PlannedChange<'a> {
    Speed(&'a SpeedCurve),
    Jump(Position),
}
