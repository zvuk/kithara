#![forbid(unsafe_code)]

//! Player-level events and supporting types.
//!
//! Moved from `kithara-play` so that all event types live in one crate.

use std::{cmp, hash, ops};

use derivative::Derivative;
use kithara_platform::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlayerStatus {
    #[default]
    Unknown,
    ReadyToPlay,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TimeControlStatus {
    #[default]
    Paused,
    WaitingToPlay,
    Playing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WaitingReason {
    ToMinimizeStalls,
    EvaluatingBufferingRate,
    NoItemToPlay,
    InterruptedBySession,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemStatus {
    #[default]
    Unknown,
    ReadyToPlay,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(u64);

impl SlotId {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Derivative, PartialEq)]
#[derivative(Default)]
pub struct MediaTime {
    value: i64,
    #[derivative(Default(value = "1"))]
    timescale: i32,
}

impl MediaTime {
    pub const ZERO: Self = Self {
        value: 0,
        timescale: 1,
    };
    pub const INVALID: Self = Self {
        value: 0,
        timescale: 0,
    };
    pub const POSITIVE_INFINITY: Self = Self {
        value: i64::MAX,
        timescale: 1,
    };

    #[must_use]
    pub fn new(value: i64, timescale: i32) -> Self {
        Self { value, timescale }
    }

    #[must_use]
    #[expect(clippy::cast_possible_truncation)]
    pub fn with_seconds(seconds: f64, timescale: i32) -> Self {
        Self {
            value: (seconds * f64::from(timescale)) as i64,
            timescale,
        }
    }

    const DURATION_TIMESCALE: i32 = 600;

    #[must_use]
    pub fn with_duration(duration: Duration) -> Self {
        Self::with_seconds(duration.as_secs_f64(), Self::DURATION_TIMESCALE)
    }

    #[must_use]
    pub fn value(&self) -> i64 {
        self.value
    }

    #[must_use]
    pub fn timescale(&self) -> i32 {
        self.timescale
    }

    #[must_use]
    #[expect(clippy::cast_precision_loss)]
    pub fn seconds(&self) -> f64 {
        if self.timescale == 0 {
            return 0.0;
        }
        self.value as f64 / f64::from(self.timescale)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.timescale > 0
    }

    #[must_use]
    pub fn is_indefinite(&self) -> bool {
        self.value == i64::MAX
    }

    #[must_use]
    pub fn to_duration(&self) -> Option<Duration> {
        if !self.is_valid() || self.is_indefinite() {
            return None;
        }
        Some(Duration::from_secs_f64(self.seconds()))
    }
}

impl Eq for MediaTime {}

impl hash::Hash for MediaTime {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
        self.timescale.hash(state);
    }
}

impl From<Duration> for MediaTime {
    fn from(d: Duration) -> Self {
        Self::with_duration(d)
    }
}

impl PartialOrd for MediaTime {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MediaTime {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        let lhs = i128::from(self.value) * i128::from(other.timescale);
        let rhs = i128::from(other.value) * i128::from(self.timescale);
        lhs.cmp(&rhs)
    }
}

impl ops::Add for MediaTime {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        if self.timescale == rhs.timescale {
            return Self::new(self.value + rhs.value, self.timescale);
        }
        let ts = self.timescale.max(rhs.timescale);
        Self::with_seconds(self.seconds() + rhs.seconds(), ts)
    }
}

impl ops::Sub for MediaTime {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        if self.timescale == rhs.timescale {
            return Self::new(self.value - rhs.value, self.timescale);
        }
        let ts = self.timescale.max(rhs.timescale);
        Self::with_seconds(self.seconds() - rhs.seconds(), ts)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TimeRange {
    pub start: Duration,
    pub duration: Duration,
}

impl TimeRange {
    #[must_use]
    pub fn new(start: Duration, duration: Duration) -> Self {
        Self { start, duration }
    }

    #[must_use]
    pub fn end(&self) -> Duration {
        self.start + self.duration
    }

    #[must_use]
    pub fn contains(&self, time: Duration) -> bool {
        time >= self.start && time < self.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PortType {
    BuiltInSpeaker,
    BuiltInReceiver,
    Headphones,
    BluetoothA2dp,
    BluetoothHfp,
    BluetoothLe,
    UsbAudio,
    Hdmi,
    AirPlay,
    LineOut,
    CarAudio,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PortDescription {
    pub port_type: PortType,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RouteDescription {
    pub outputs: Vec<PortDescription>,
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

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct BpmInfo {
    pub bpm: f64,
    pub confidence: f32,
    pub first_beat_offset: Duration,
}

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
    ItemDidPlayToEnd,
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
