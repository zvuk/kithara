#![forbid(unsafe_code)]

use std::{cmp, hash, ops};

use kithara_platform::{sync::Arc, time::Duration};
use num_traits::cast::{AsPrimitive, ToPrimitive};

use crate::SlotId;

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

#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct MediaTime {
    timescale: i32,
    value: i64,
}

impl Default for MediaTime {
    fn default() -> Self {
        Self {
            timescale: 1,
            value: 0,
        }
    }
}

impl MediaTime {
    const DURATION_TIMESCALE: i32 = 600;
    /// Anchor for "indefinite" media times: an `f64`→`i64` conversion
    /// overflow or a `NaN` input is mapped to this value. Queried via
    /// [`MediaTime::is_indefinite`]; carried by [`MediaTime::POSITIVE_INFINITY`].
    pub const INDEFINITE_VALUE: i64 = i64::MAX;
    pub const INVALID: Self = Self {
        value: 0,
        timescale: 0,
    };

    pub const POSITIVE_INFINITY: Self = Self {
        value: Self::INDEFINITE_VALUE,
        timescale: 1,
    };

    pub const ZERO: Self = Self {
        value: 0,
        timescale: 1,
    };

    #[must_use]
    pub fn new(value: i64, timescale: i32) -> Self {
        Self { timescale, value }
    }

    #[must_use]
    pub fn is_indefinite(&self) -> bool {
        self.value == Self::INDEFINITE_VALUE
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.timescale > 0
    }

    #[must_use]
    pub fn seconds(&self) -> f64 {
        if self.timescale == 0 {
            return 0.0;
        }
        let value_f64: f64 = self.value.as_();
        value_f64 / f64::from(self.timescale)
    }

    #[must_use]
    pub fn with_duration(duration: Duration) -> Self {
        Self::with_seconds(duration.as_secs_f64(), Self::DURATION_TIMESCALE)
    }

    #[must_use]
    pub fn with_seconds(seconds: f64, timescale: i32) -> Self {
        let value = (seconds * f64::from(timescale))
            .to_i64()
            .unwrap_or(Self::INDEFINITE_VALUE);
        Self { timescale, value }
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

impl TryFrom<&MediaTime> for Duration {
    type Error = ();

    fn try_from(t: &MediaTime) -> Result<Self, Self::Error> {
        if !t.is_valid() || t.is_indefinite() {
            return Err(());
        }
        Ok(Self::from_secs_f64(t.seconds()))
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
    pub duration: Duration,
    pub start: Duration,
}

impl TimeRange {
    #[must_use]
    pub fn new(start: Duration, duration: Duration) -> Self {
        Self { duration, start }
    }

    #[must_use]
    pub fn contains(&self, time: Duration) -> bool {
        time >= self.start && time < self.end()
    }

    #[must_use]
    pub fn end(&self) -> Duration {
        self.start + self.duration
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
    pub first_beat_offset: Duration,
    pub confidence: Option<f32>,
    pub bpm: f64,
}

impl BpmInfo {
    #[must_use]
    pub fn new(bpm: f64, confidence: Option<f32>, first_beat_offset: Duration) -> Self {
        Self {
            first_beat_offset,
            confidence,
            bpm,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StretchBackendKind {
    Signalsmith,
    Bungee,
    Unknown,
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
    PlaybackStarted {
        src: Arc<str>,
        item_id: Option<Arc<str>>,
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
    /// A track reached natural end-of-stream. `src` is the underlying
    /// audio source identifier of the track that ended — necessary so
    /// consumers can distinguish a genuine final-track EOF from a stale
    /// outgoing-track EOF that fires after a crossfade has already
    /// promoted the next track. `item_id` is the optional caller-side
    /// item identifier (FFI bindings tag tracks with stable UUIDs;
    /// internal callers may leave it `None`).
    ItemDidPlayToEnd {
        src: Arc<str>,
        item_id: Option<Arc<str>>,
    },
    /// A track aborted mid-stream because the underlying decoder /
    /// source reported a non-recoverable error. Distinct from
    /// [`ItemDidPlayToEnd`](Self::ItemDidPlayToEnd): the track did
    /// NOT reach its natural end and queue consumers must treat this
    /// as a track-failure signal (skip-and-flag) rather than a
    /// normal auto-advance.
    ItemDidFail {
        src: Arc<str>,
        item_id: Option<Arc<str>>,
    },
    /// Leading track entered the prefetch window — arm the next slot.
    PrefetchRequested,
    /// Leading track entered the crossfade window — commit the armed slot.
    /// Suppressed when `crossfade_duration == 0` (audio thread handles
    /// handover at EOF).
    HandoverRequested,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ItemEvent {
    PlaybackLikelyToKeepUp,
    PlaybackStalled,
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

/// Facts committed by the session transport owner.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TransportEvent {
    TempoCommitted {
        beats_per_minute: f64,
        revision: u64,
    },
    PlayStateCommitted {
        playing: bool,
        revision: u64,
    },
    SeekCommitted {
        position_beats: f64,
        revision: u64,
    },
    Failed {
        revision: Option<u64>,
        reason: String,
    },
}

/// Audible movement through a track's beat map.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaybackDirection {
    /// Session beats advance toward higher track beats.
    #[default]
    Forward,
    /// Session beats advance toward lower track beats.
    Reverse,
}

/// Facts committed by the track synchronization owner.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SyncEvent {
    BindingCommitted {
        slot: SlotId,
        session_anchor_beats: f64,
        track_anchor_beats: f64,
        direction: PlaybackDirection,
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
    KeylockChanged {
        on: bool,
    },
    StretchBackendChanged {
        kind: StretchBackendKind,
    },
}
