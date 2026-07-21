use std::{fmt, num::NonZeroU64};

const SECONDS_PER_MINUTE: f64 = 60.0;

/// A finite, positive musical tempo in beats per minute.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Tempo(f64);

impl Tempo {
    /// Creates a tempo, rejecting non-finite and non-positive values.
    pub fn new(beats_per_minute: f64) -> Result<Self, TempoError> {
        if beats_per_minute.is_finite() && beats_per_minute > 0.0 {
            Ok(Self(beats_per_minute))
        } else {
            Err(TempoError { beats_per_minute })
        }
    }

    #[must_use]
    /// Returns the tempo in beats per minute.
    pub fn beats_per_minute(self) -> f64 {
        self.0
    }

    pub(crate) fn beats_per_second(self) -> f64 {
        self.0 / SECONDS_PER_MINUTE
    }
}

impl TryFrom<f64> for Tempo {
    type Error = TempoError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// The value supplied for a musical tempo was invalid.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("tempo must be finite and positive, got {beats_per_minute}")]
#[non_exhaustive]
pub struct TempoError {
    beats_per_minute: f64,
}

/// A continuous beat coordinate on the session transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct SessionBeat(f64);

impl SessionBeat {
    /// Creates a finite session-beat coordinate. Negative beats are valid.
    pub fn new(value: f64) -> Result<Self, SessionBeatError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(SessionBeatError { value })
        }
    }

    #[must_use]
    /// Returns the continuous beat coordinate.
    pub fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for SessionBeat {
    type Error = SessionBeatError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// The value supplied for a session-beat coordinate was invalid.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("session beat must be finite, got {value}")]
#[non_exhaustive]
pub struct SessionBeatError {
    value: f64,
}

/// Monotonic generation of a committed session transport configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct TransportRevision(NonZeroU64);

impl TransportRevision {
    pub(crate) const FIRST: Self = Self(NonZeroU64::MIN);

    pub(crate) fn checked_next(self) -> Option<Self> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }

    #[must_use]
    /// Returns the serialized revision value.
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    #[cfg(test)]
    pub(crate) const fn new_for_test(value: u64) -> Self {
        match NonZeroU64::new(value) {
            Some(value) => Self(value),
            None => panic!("transport test revision must be non-zero"),
        }
    }
}

impl fmt::Display for TransportRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

/// The last session transport position processed by the audio graph.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct SessionTransportSnapshot {
    position: SessionBeat,
    playing: bool,
    tempo: Tempo,
    revision: TransportRevision,
}

impl SessionTransportSnapshot {
    pub(crate) fn new(
        position: SessionBeat,
        playing: bool,
        tempo: Tempo,
        revision: TransportRevision,
    ) -> Self {
        Self {
            position,
            playing,
            tempo,
            revision,
        }
    }

    #[must_use]
    /// Returns whether the processed session transport is playing.
    pub fn is_playing(self) -> bool {
        self.playing
    }

    #[must_use]
    /// Returns the processed position on the session beat grid.
    pub fn position(self) -> SessionBeat {
        self.position
    }

    #[must_use]
    /// Returns the monotonic revision of the committed transport configuration.
    pub fn revision(self) -> TransportRevision {
        self.revision
    }

    #[must_use]
    /// Returns the tempo that produced this processed position.
    pub fn tempo(self) -> Tempo {
        self.tempo
    }
}
