#![forbid(unsafe_code)]

use std::{cmp, hash, ops};

use kithara_events::{Event, TrackId};
use kithara_platform::time::Duration;
use kithara_render::bridge::PlaybackFault;
use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::{ItemRole, PlayerStatus, PortType, TimeControlStatus, WaitingReason};

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

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PortDescription {
    pub port_type: PortType,
    pub name: String,
    pub uid: String,
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
    pub const fn new(bpm: f64, confidence: Option<f32>, first_beat_offset: Duration) -> Self {
        Self {
            first_beat_offset,
            confidence,
            bpm,
        }
    }
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
    /// on the role, and never on the [`super::TrackRef::src`] inside it.
    ItemDidPlayToEnd {
        item: ItemRole,
    },
    /// A track aborted mid-stream because the underlying decoder /
    /// source reported a non-recoverable error, or because the render
    /// context could not serve it. Distinct from
    /// [`ItemDidPlayToEnd`](Self::ItemDidPlayToEnd): the track did
    /// NOT reach its natural end and queue consumers must treat this
    /// as a track-failure signal (skip-and-flag) rather than a
    /// normal auto-advance. `fault` names which defect ended it.
    ItemDidFail {
        item: ItemRole,
        fault: PlaybackFault,
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
            "kithara_play::api::event::player::PlayerEvent"
        );
    }
}
