use std::time::Duration;

use crate::dj::bpm::BpmInfo;
use crate::session::RouteDescription;
use crate::time::MediaTime;
use crate::types::{ItemStatus, PlayerStatus, SlotId, TimeControlStatus, TimeRange, WaitingReason};

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PlayerEvent {
    StatusChanged {
        status: PlayerStatus,
    },
    TimeControlStatusChanged {
        status: TimeControlStatus,
        reason: Option<WaitingReason>,
    },
    RateChanged {
        rate: f32,
    },
    VolumeChanged {
        volume: f32,
    },
    MuteChanged {
        muted: bool,
    },
    CurrentItemChanged,
    PrerollCompleted {
        success: bool,
    },
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ItemEvent {
    StatusChanged { status: ItemStatus },
    DurationChanged { duration: MediaTime },
    LoadedTimeRangesChanged { ranges: Vec<TimeRange> },
    PlaybackLikelyToKeepUp,
    PlaybackUnlikelyToKeepUp,
    PlaybackBufferEmpty,
    PlaybackBufferFull,
    DidPlayToEnd,
    FailedToPlayToEnd { error: String },
    PlaybackStalled,
    TimeJumped,
    SeekCompleted { success: bool },
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum EngineEvent {
    Started,
    Stopped,
    SlotAllocated {
        slot: SlotId,
    },
    SlotReleased {
        slot: SlotId,
    },
    CrossfadeStarted {
        from: SlotId,
        to: SlotId,
        duration: Duration,
    },
    CrossfadeProgress {
        from: SlotId,
        to: SlotId,
        progress: f32,
    },
    CrossfadeCompleted {
        from: SlotId,
        to: SlotId,
    },
    CrossfadeCancelled,
    MasterVolumeChanged {
        volume: f32,
    },
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SessionEvent {
    Interruption {
        kind: InterruptionKind,
    },
    RouteChanged {
        reason: RouteChangeReason,
        previous_route: RouteDescription,
    },
    MediaServicesLost,
    MediaServicesReset,
    SilenceSecondaryAudioHint {
        should_silence: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InterruptionKind {
    Began,
    Ended { should_resume: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RouteChangeReason {
    Unknown,
    NewDeviceAvailable,
    OldDeviceUnavailable,
    CategoryChange,
    Override,
    WakeFromSleep,
    NoSuitableRouteForCategory,
    RouteConfigurationChange,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DjEvent {
    BpmDetected {
        slot: SlotId,
        info: BpmInfo,
    },
    BeatTick {
        slot: SlotId,
        beat_number: u64,
        timestamp: MediaTime,
    },
    BpmSyncEngaged {
        leader: SlotId,
        follower: SlotId,
    },
    BpmSyncDisengaged {
        slot: SlotId,
    },
    PhaseAligned {
        leader: SlotId,
        follower: SlotId,
        offset_beats: f64,
    },
}
