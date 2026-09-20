#![forbid(unsafe_code)]

use std::{cmp, hash, ops};

use kithara_events::{Event, SlotId, TrackId};
use kithara_platform::{sync::Arc, time::Duration};
use num_traits::cast::{AsPrimitive, ToPrimitive};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PlayerStatus {
    #[default]
    Unknown,
    ReadyToPlay,
    Failed,
}

/// Where an item sits in the player's arena and what it renders,
/// alongside the identity the queue gave it.
///
/// A slot is a processor holding an arena of items, not a single item:
/// arming the successor loads it into the *current* slot, and a crossfade
/// promotes it there (`CrossfadeStarted { from: slot, to: slot }`). So
/// `slot` alone does not say which item this is, and `src` names a
/// rendered resource that two queue entries may share. `id` is the only
/// part that answers "which entry"; the other two are what makes a log
/// line readable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, derive_more::Display)]
#[display("{id}@slot{} {src}", slot.value())]
#[non_exhaustive]
pub struct TrackRef {
    /// The rendered resource behind the item. Not an identity: a playlist
    /// may repeat one URL.
    pub src: Arc<str>,
    /// The processor slot the item is loaded into.
    pub slot: SlotId,
    /// The queue's identity for this item, set when it was handed to the
    /// player. The same value the FFI reports as `audioId`.
    pub id: TrackId,
}

impl TrackRef {
    #[must_use]
    pub const fn new(id: TrackId, slot: SlotId, src: Arc<str>) -> Self {
        Self { src, slot, id }
    }
}

/// An item in the player's arena, named together with the role it holds
/// there. Only the player can fill this in.
///
/// The subject of a player event is placed by two answers — which slot it
/// came from, and which item inside that slot — and `src` answers neither:
/// it names a rendered resource, not a queue entry.
///
/// [`TrackRef`] lives *inside* the role rather than beside it, so a
/// consumer has to say which item it is holding before it can use its
/// identity — the omission that once let a background slot's end advance
/// the queue.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemRole {
    /// The item the listener is hearing. The only role that drives
    /// auto-advance.
    Leading(TrackRef),
    /// The outgoing half of a crossfade: still inside the current slot,
    /// but the incoming item has already been promoted over it. Its end
    /// is expected and carries no instruction.
    Outgoing(TrackRef),
    /// An item in a slot the phase no longer holds — an orphan draining
    /// the last of its notifications until it is unregistered, while a
    /// different item plays. Acting on it cuts an item still going.
    Background(TrackRef),
}

impl ItemRole {
    /// The queue's identity for this item.
    #[must_use]
    pub const fn id(&self) -> TrackId {
        self.track().id
    }

    /// Whether this is the item being heard, and so the one that should
    /// drive auto-advance.
    #[must_use]
    pub const fn is_leading(&self) -> bool {
        matches!(self, Self::Leading(_))
    }

    /// The item this role is about.
    #[must_use]
    pub const fn track(&self) -> &TrackRef {
        match self {
            Self::Leading(track) | Self::Outgoing(track) | Self::Background(track) => track,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TimeControlStatus {
    #[default]
    Paused,
    WaitingToPlay,
    Playing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaitingReason {
    ToMinimizeStalls,
    EvaluatingBufferingRate,
    NoItemToPlay,
    InterruptedBySession,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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
    pub const fn new(value: i64, timescale: i32) -> Self {
        Self { timescale, value }
    }

    #[must_use]
    pub const fn is_indefinite(&self) -> bool {
        self.value == Self::INDEFINITE_VALUE
    }

    #[must_use]
    pub const fn is_valid(&self) -> bool {
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
    pub const fn new(start: Duration, duration: Duration) -> Self {
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
pub enum InterruptionKind {
    Began,
    Ended { should_resume: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// Where the beat the grid numbers zero sounds, from the track start.
    pub first_beat_offset: Duration,
    pub confidence: Option<f32>,
    pub bpm: f64,
}

impl BpmInfo {
    #[must_use]
    pub const fn new(bpm: f64, confidence: Option<f32>, first_beat_offset: Duration) -> Self {
        Self {
            first_beat_offset,
            confidence,
            bpm,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StretchBackendKind {
    Signalsmith,
    Bungee,
    Unknown,
}

#[derive(Clone, Debug, Event)]
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
    /// An item began rendering. Published for whichever slot's item
    /// entered `Playing`, so it carries [`ItemRole`] for the same reason
    /// [`ItemDidPlayToEnd`](Self::ItemDidPlayToEnd) does: a slot the phase
    /// no longer holds can start its own item while a different one is
    /// being heard.
    PlaybackStarted {
        item: ItemRole,
    },
    VolumeChanged {
        volume: f32,
    },
    MuteChanged {
        muted: bool,
    },
    CurrentItemChanged {
        item: Option<TrackId>,
    },
    PrerollCompleted {
        success: bool,
    },
    /// An item reached natural end-of-stream. [`ItemRole`] is the
    /// player's own answer to *which* item this was; auto-advance must key
    /// on the role, and never on the [`TrackRef::src`] inside it.
    ItemDidPlayToEnd {
        item: ItemRole,
    },
    /// A track aborted mid-stream because the underlying decoder /
    /// source reported a non-recoverable error. Distinct from
    /// [`ItemDidPlayToEnd`](Self::ItemDidPlayToEnd): the track did
    /// NOT reach its natural end and queue consumers must treat this
    /// as a track-failure signal (skip-and-flag) rather than a
    /// normal auto-advance.
    ItemDidFail {
        item: ItemRole,
    },
    /// Leading track entered the prefetch window — arm the next slot.
    PrefetchRequested,
    /// A track entered its crossfade window — commit the armed slot.
    /// Suppressed when `crossfade_duration == 0` (audio thread handles
    /// handover at EOF).
    ///
    /// Carries [`ItemRole`] for the same reason
    /// [`ItemDidPlayToEnd`](Self::ItemDidPlayToEnd) does: the request names
    /// the track that is running out, and a consumer whose cursor has
    /// already moved to the successor must be able to see that this
    /// handover was already performed.
    HandoverRequested {
        item: ItemRole,
    },
}

#[derive(Clone, Debug, Event)]
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

#[derive(Clone, Debug, Event)]
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

/// Audible movement through a track's beat map.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaybackDirection {
    /// Session beats advance toward higher track beats.
    #[default]
    Forward,
    /// Session beats advance toward lower track beats.
    Reverse,
}

#[derive(Clone, Debug, Event)]
pub enum DjEvent {
    BpmDetected {
        slot: SlotId,
        info: BpmInfo,
    },
    BeatTick {
        slot: SlotId,
        /// The ordinal the beat grid gives this beat. Zero is the first beat
        /// the analysis marked, so beats the grid extends before it are
        /// negative.
        beat_number: i64,
        timestamp: MediaTime,
    },
    KeylockChanged {
        on: bool,
    },
    StretchBackendChanged {
        kind: StretchBackendKind,
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn player_event_is_owned_by_kithara_play() {
        assert_eq!(
            ::core::any::type_name::<PlayerEvent>(),
            "kithara_play::api::event::PlayerEvent"
        );
    }
}
