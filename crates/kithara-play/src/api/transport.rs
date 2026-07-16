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

/// Typed reason why a slot could not join one atomic transport change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TransportPreparationFailure {
    #[error("the proposed presentation boundary expired")]
    BoundaryExpired,
    #[error("participants proposed different commit boundaries")]
    BoundaryMismatch,
    #[error("a bound track is unavailable")]
    BindingUnavailable,
    #[error("the participant command lane is unavailable")]
    CommandLaneUnavailable,
    #[error("required look-ahead is unavailable")]
    LookAheadUnavailable,
    #[error("slot membership changed")]
    MembershipChanged,
    #[error("the renderer is unavailable")]
    RendererUnavailable,
    #[error("the audio route was invalidated")]
    RouteInvalidated,
    #[error("the participant returned a stale revision")]
    StaleRevision,
    #[error("the requested elastic rate is unsupported")]
    UnsupportedRate,
}

/// The last session transport position processed by the audio graph.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct SessionTransportSnapshot {
    position: SessionBeat,
    playing: bool,
    tempo: Tempo,
    revision: u64,
}

impl SessionTransportSnapshot {
    pub(crate) fn new(position: SessionBeat, playing: bool, tempo: Tempo, revision: u64) -> Self {
        Self {
            position,
            playing,
            tempo,
            revision,
        }
    }

    #[must_use]
    /// Returns the processed position on the session beat grid.
    pub fn position(self) -> SessionBeat {
        self.position
    }

    #[must_use]
    /// Returns whether the processed session transport is playing.
    pub fn is_playing(self) -> bool {
        self.playing
    }

    #[must_use]
    /// Returns the tempo that produced this processed position.
    pub fn tempo(self) -> Tempo {
        self.tempo
    }

    #[must_use]
    /// Returns the monotonic revision of the committed transport configuration.
    pub fn revision(self) -> u64 {
        self.revision
    }
}
