use std::num::NonZeroU64;

use kithara_audio::{CoordinateError, SessionBeat};

const SECONDS_PER_MINUTE: f64 = 60.0;

/// A musical tempo in beats per minute, inside the range the session clock can
/// carry.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct Tempo(
    /// Returns the tempo in beats per minute.
    #[field(get = beats_per_minute, copy)]
    f64,
);

impl Tempo {
    /// The slowest accepted tempo.
    pub const MIN_BEATS_PER_MINUTE: f64 = 1.0;

    /// The fastest accepted tempo. The upper bound is what keeps the anchor
    /// arithmetic finite: an unbounded tempo overflows the beat span of a
    /// single block, and the resulting failure strands the transport with an
    /// active commit and no anchor.
    pub const MAX_BEATS_PER_MINUTE: f64 = 1_000.0;

    /// Creates a tempo, rejecting non-finite and out-of-range values.
    pub fn new(beats_per_minute: f64) -> Result<Self, TempoError> {
        if (Self::MIN_BEATS_PER_MINUTE..=Self::MAX_BEATS_PER_MINUTE).contains(&beats_per_minute) {
            Ok(Self(beats_per_minute))
        } else {
            Err(TempoError { beats_per_minute })
        }
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
#[error(
    "tempo must be between {min} and {max} beats per minute, got {beats_per_minute}",
    min = Tempo::MIN_BEATS_PER_MINUTE,
    max = Tempo::MAX_BEATS_PER_MINUTE,
)]
#[non_exhaustive]
pub struct TempoError {
    beats_per_minute: f64,
}

/// The session-beat grid a start snaps to, counted in session beats: `1.0` is
/// the next beat, `4.0` the next bar of four, `16.0` the next phrase.
///
/// A caller names the grid rather than the beat because the beat it would name
/// is the transport's own position plus a rounding, read at a moment it cannot
/// observe atomically. The deck resolves the quantum against the same commit it
/// binds to, so the answer cannot be stale.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, derive_more::Into)]
pub struct BeatQuantum(f64);

impl BeatQuantum {
    /// Creates a quantum from a finite, strictly positive number of beats.
    ///
    /// # Errors
    ///
    /// Returns [`BeatQuantumError`] when `beats` is not finite or not positive:
    /// neither names a grid.
    pub fn new(beats: f64) -> Result<Self, BeatQuantumError> {
        if beats.is_finite() && beats > 0.0 {
            Ok(Self(beats))
        } else {
            Err(BeatQuantumError { beats })
        }
    }
}

impl TryFrom<f64> for BeatQuantum {
    type Error = BeatQuantumError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// The value supplied for a beat quantum was invalid.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("beat quantum must be finite and positive, got {beats}")]
#[non_exhaustive]
pub struct BeatQuantumError {
    beats: f64,
}

/// Monotonic generation of a committed session transport configuration.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    derive_more::Display,
    derive_more::Into,
)]
#[display("{_0}")]
#[into(u64)]
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
}

/// The last session transport position processed by the audio graph.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SessionTransportSnapshot {
    /// Returns the processed position on the session beat grid.
    #[field(get, copy)]
    position: SessionBeat,
    /// Returns the tempo that produced this processed position.
    #[field(get, copy)]
    tempo: Tempo,
    /// Returns the monotonic revision of the committed transport configuration.
    #[field(get, copy)]
    revision: TransportRevision,
    /// Returns whether the processed session transport is playing.
    #[field(get = is_playing, copy)]
    playing: bool,
}

impl SessionTransportSnapshot {
    pub(crate) const fn new(
        position: SessionBeat,
        playing: bool,
        tempo: Tempo,
        revision: TransportRevision,
    ) -> Self {
        Self {
            position,
            tempo,
            revision,
            playing,
        }
    }

    /// The first beat on the `quantum` grid strictly after this position.
    ///
    /// Strictly, because a position that already sits on the grid names a
    /// frame this block has passed: snapping to it would arm a start that can
    /// never be resolved.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the position and the quantum are so
    /// far apart that the next grid beat is not representable.
    pub(crate) fn next_beat_on(self, quantum: BeatQuantum) -> Result<SessionBeat, CoordinateError> {
        let step = f64::from(quantum);
        let position = f64::from(self.position);
        SessionBeat::new((position / step).floor().mul_add(step, step))
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{BeatQuantum, SessionBeat, SessionTransportSnapshot, Tempo, TransportRevision};

    fn snapshot(position: f64) -> SessionTransportSnapshot {
        SessionTransportSnapshot::new(
            SessionBeat::new(position).expect("invariant: the fixture position is finite"),
            true,
            Tempo::new(120.0).expect("invariant: the fixture tempo is in range"),
            TransportRevision::FIRST,
        )
    }

    fn bar() -> BeatQuantum {
        BeatQuantum::new(4.0).expect("invariant: four beats is a grid")
    }

    /// A position already on the grid names a frame this block has passed, so
    /// the start snaps to the following bar rather than to the one underfoot.
    #[kithara::test]
    fn a_position_on_the_grid_snaps_to_the_next_one() {
        let next = snapshot(8.0)
            .next_beat_on(bar())
            .expect("invariant: the next bar is representable");

        assert_eq!(f64::from(next), 12.0);
    }

    /// Between grid beats the start snaps up to the coming one, never back to
    /// the one already played.
    #[kithara::test]
    fn a_position_between_grid_beats_snaps_up() {
        let next = snapshot(9.3)
            .next_beat_on(bar())
            .expect("invariant: the next bar is representable");

        assert_eq!(f64::from(next), 12.0);
    }

    /// Before beat zero the grid still runs backwards, so a transport that has
    /// not reached its origin arms on the coming negative bar line.
    #[kithara::test]
    fn a_negative_position_snaps_up_to_the_coming_grid_beat() {
        let next = snapshot(-9.3)
            .next_beat_on(bar())
            .expect("invariant: the next bar is representable");

        assert_eq!(f64::from(next), -8.0);
    }

    #[kithara::test]
    fn a_quantum_must_be_positive_and_finite() {
        assert!(BeatQuantum::new(0.0).is_err());
        assert!(BeatQuantum::new(-4.0).is_err());
        assert!(BeatQuantum::new(f64::NAN).is_err());
    }

    #[kithara::test]
    fn accepts_negative_and_zero_coordinates() {
        let negative = SessionBeat::new(-1.5).expect("invariant: finite negative beat is valid");
        let zero = SessionBeat::new(0.0).expect("invariant: zero beat is valid");

        assert_eq!(f64::from(negative), -1.5);
        assert_eq!(f64::from(zero), 0.0);
    }

    #[kithara::test]
    fn rejects_non_finite_coordinates() {
        assert!(SessionBeat::new(f64::NAN).is_err());
        assert!(SessionBeat::new(f64::INFINITY).is_err());
        assert!(SessionBeat::new(f64::NEG_INFINITY).is_err());
    }
}
