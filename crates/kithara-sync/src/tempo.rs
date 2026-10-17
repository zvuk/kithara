use kithara_derive::Ranged;

/// A musical tempo in beats per minute, inside the range the session clock can
/// carry.
///
/// The upper bound is what keeps the anchor arithmetic finite: an unbounded
/// tempo overflows the beat span of a single block, and the transport could
/// place no beat anchor on it.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, fieldwork::Fieldwork, Ranged)]
#[fieldwork(get)]
#[ranged(min = 1.0, max = 1_000.0, default = 120.0)]
pub struct Tempo(
    /// Returns the tempo in beats per minute.
    #[field(get = beats_per_minute, copy)]
    f64,
);

impl Tempo {
    /// Creates a tempo, rejecting non-finite and out-of-range values.
    ///
    /// # Errors
    /// Returns [`TempoError`] for non-finite values or values outside the supported range.
    pub fn new(beats_per_minute: f64) -> Result<Self, TempoError> {
        Self::checked(beats_per_minute).ok_or(TempoError { beats_per_minute })
    }

    #[must_use]
    pub fn beats_per_second(self) -> f64 {
        const SECONDS_PER_MINUTE: f64 = 60.0;

        f64::from(self) / SECONDS_PER_MINUTE
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
#[error(
    "tempo must be between {min} and {max} beats per minute, got {beats_per_minute}",
    min = f64::from(Tempo::MIN),
    max = f64::from(Tempo::MAX),
)]
#[non_exhaustive]
pub struct TempoError {
    beats_per_minute: f64,
}
