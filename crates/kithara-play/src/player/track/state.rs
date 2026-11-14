use std::{
    cell::Cell,
    collections::VecDeque,
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
};

use kithara_abr::AbrHandle;
use kithara_command::{Receipt, Sender, Seq, When};
use kithara_decode::TrackMetadata;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_render::{
    CrossfadeSettings, LaneCommand, LaneFrame, LaneId, LaneProtocol,
    bridge::{FadeDir, PlaybackFault, Slot, SlotMark, TrackClock},
    rt::track::PlayerResource,
};
use kithara_signal::{AudioChunkInfo, FrameCount, SegmentId, SessionFrame};
use kithara_sync::SpeedCurve;

use super::super::settings::TrackSettingsChange;
use crate::{ResourceLoad, Settled};

/// A position in a track's media.
pub type Position = Duration;

/// What a track is told to do.
pub enum TrackCommand<S> {
    GainProgram(kithara_render::bridge::SlotGainProgram),
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
    /// Sets the clock the track keeps through a PCM dropout, for its seated
    /// slot and every later seat.
    Clock(TrackClock),
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
///
/// A track carries no effects; they belong to its deck:
/// ```compile_fail
/// fn count(snapshot: &kithara_play::TrackSnapshot) -> usize {
///     snapshot.effects.len()
/// }
/// ```
#[derive(Clone)]
pub struct TrackSnapshot {
    pub output: kithara_render::bridge::SlotOutputSnapshot,
    pub item: TrackId,
    /// None while a background load holds the track off the deck.
    pub slot: Option<Slot>,
    pub status: TrackStatus,
    /// The speed at the last consumed frame.
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

/// Where the track's media lies on its source's raw clock.
#[derive(Clone, Copy, Default)]
pub(super) struct Extent {
    /// End of the track on the media clock.
    pub(super) duration: Option<Duration>,
    /// Raw source time of content zero.
    pub(super) media_origin: Option<Duration>,
}

impl Extent {
    pub(super) fn of(source: Option<AudioChunkInfo>) -> Self {
        Self {
            duration: source.and_then(|info| info.media_end),
            media_origin: source.and_then(|info| info.media_origin),
        }
    }
}

/// Admission and dispatcher retirement of one source channel.
#[derive(Default)]
pub(super) enum Lane {
    #[default]
    None,
    Opening(Sender<LaneProtocol>),
    Registered {
        id: LaneId,
        sender: Sender<LaneProtocol>,
    },
    RetiringOpen(Sender<LaneProtocol>),
    Retiring {
        id: LaneId,
        sender: Sender<LaneProtocol>,
        release: Option<Seq>,
    },
}

impl Lane {
    pub(super) fn sender(&self) -> Option<&Sender<LaneProtocol>> {
        match self {
            Self::Opening(sender) | Self::Registered { sender, .. } => Some(sender),
            _ => None,
        }
    }

    pub(super) fn sender_mut(&mut self) -> Option<&mut Sender<LaneProtocol>> {
        match self {
            Self::Opening(sender) | Self::Registered { sender, .. } => Some(sender),
            _ => None,
        }
    }

    pub(super) fn available(&self) -> usize {
        self.sender().map_or(0, Sender::available)
    }

    pub(super) fn receipt(&mut self) -> Option<Receipt<LaneProtocol>> {
        match self {
            Self::None => None,
            Self::Opening(sender)
            | Self::Registered { sender, .. }
            | Self::RetiringOpen(sender)
            | Self::Retiring { sender, .. } => sender.receipts().next(),
        }
    }

    pub(super) fn resident(&self) -> Option<LaneId> {
        match self {
            Self::Registered { id, .. } => Some(*id),
            _ => None,
        }
    }

    pub(super) fn loaded(&mut self, id: LaneId) {
        *self = match std::mem::take(self) {
            Self::Opening(sender) => Self::Registered { id, sender },
            Self::RetiringOpen(sender) => Self::Retiring {
                id,
                sender,
                release: None,
            },
            state => state,
        };
    }

    pub(super) fn retire(&mut self) {
        *self = match std::mem::take(self) {
            Self::Opening(sender) => Self::RetiringOpen(sender),
            Self::Registered { id, sender } => Self::Retiring {
                id,
                sender,
                release: None,
            },
            state => state,
        };
    }

    pub(super) fn retiring(&self) -> bool {
        matches!(self, Self::RetiringOpen(_) | Self::Retiring { .. })
    }

    pub(super) fn release_needed(&self) -> Option<LaneId> {
        match self {
            Self::Retiring {
                id, release: None, ..
            } => Some(*id),
            _ => None,
        }
    }

    pub(super) fn release_seq(&self) -> Option<Seq> {
        match self {
            Self::Retiring { release, .. } => *release,
            _ => None,
        }
    }

    pub(super) fn releasing(&mut self, seq: Option<Seq>) {
        if let Self::Retiring { release, .. } = self {
            *release = seq;
        }
    }
}

pub(super) struct Loading {
    pub(super) seq: Seq,
    /// The opened source's consumer, until a deck batch takes it.
    pub(super) opened: Option<Box<PlayerResource>>,
    pub(super) speed: Option<(SpeedCurve, f32)>,
}

pub(super) struct LaneOperation {
    pub(super) seq: Seq,
    pub(super) when: When<LaneFrame>,
    pub(super) segment: SegmentId,
    pub(super) command: LaneCommand,
    pub(super) applied: bool,
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

#[derive(Clone, Copy)]
pub(super) struct SpeedProjection {
    pub(super) at: LaneFrame,
    pub(super) initial_bits: u32,
    pub(super) declick: FrameCount,
    pub(super) speed: f32,
}

/// Unanswered commands and effects still needed by live segments.
///
/// `speed_projection` is derived from `commands`, so the history changes only
/// through intake and submission, which invalidate it. Producer feedback is
/// taken once by the driver, independently of retained planning effects.
#[derive(Default)]
pub(super) struct LaneHistory {
    pub(super) commands: Vec<LaneOperation>,
    pub(super) receipts: VecDeque<Settled<LaneFrame>>,
    /// Entries name every projection input other than `commands`.
    pub(super) speed_projection: Cell<Option<SpeedProjection>>,
}

impl LaneHistory {
    pub(super) fn submit(&mut self, operations: impl IntoIterator<Item = LaneOperation>) {
        self.speed_projection.set(None);
        self.commands.extend(operations);
    }

    pub(super) fn retain(&mut self, keep: impl FnMut(&LaneOperation) -> bool) {
        let before = self.commands.len();
        self.commands.retain(keep);
        if self.commands.len() != before {
            self.speed_projection.set(None);
        }
    }

    /// The operations one lane receipt answers.
    pub(super) fn answered(&mut self, seq: Seq) -> impl Iterator<Item = &mut LaneOperation> {
        self.speed_projection.set(None);
        self.commands
            .iter_mut()
            .filter(move |operation| operation.seq == seq)
    }
}

pub(super) enum PlannedChange<'a> {
    Speed(&'a SpeedCurve),
    Jump(Position),
}
