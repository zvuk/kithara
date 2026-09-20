use kithara_derive::Ranged;
use kithara_warp::{
    BeatGridSnapshot, BeatGridStamp, SessionAnchor, SessionBeat, SessionEpoch, TransportRevision,
};

/// A musical tempo in beats per minute, inside the range the session clock can
/// carry.
///
/// The upper bound is what keeps the anchor arithmetic finite: an unbounded
/// tempo overflows the beat span of a single block, and the resulting failure
/// strands the transport with an active commit and no anchor.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, fieldwork::Fieldwork, Ranged)]
#[fieldwork(get)]
#[ranged(min = 1.0, max = 1_000.0)]
pub struct Tempo(
    /// Returns the tempo in beats per minute.
    #[field(get = beats_per_minute, copy)]
    f64,
);

impl Tempo {
    /// Creates a tempo, rejecting non-finite and out-of-range values.
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

/// The last session transport position processed by the audio graph.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SessionTransportSnapshot {
    /// Returns the exact session-grid identity and geometry revision.
    #[field(get, copy)]
    session_grid_stamp: BeatGridStamp,
    /// Session-clock relation used to construct the public session-grid view.
    #[field(skip)]
    anchor: SessionAnchor,
    /// Returns the processed position on the session beat grid.
    #[field(get, copy)]
    position: SessionBeat,
    /// Returns the session-grid generation defining the live frame axis.
    #[field(get, copy)]
    session_epoch: SessionEpoch,
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
    #[must_use]
    pub const fn new(
        position: SessionBeat,
        playing: bool,
        tempo: Tempo,
        revision: TransportRevision,
        anchor: SessionAnchor,
        session_grid_stamp: BeatGridStamp,
        session_epoch: SessionEpoch,
    ) -> Self {
        Self {
            session_grid_stamp,
            anchor,
            position,
            session_epoch,
            tempo,
            revision,
            playing,
        }
    }

    /// Returns the exact session-clock anchor carried by this observation.
    #[doc(hidden)]
    #[must_use]
    pub const fn anchor(self) -> SessionAnchor {
        self.anchor
    }

    /// Builds a read-only session grid from this single atomic observation.
    ///
    /// Construction happens on the control side after reading the Copy-only
    /// transport snapshot; the audio callback never publishes or drops an
    /// allocated grid handle.
    #[must_use]
    pub fn session_grid(self) -> BeatGridSnapshot {
        BeatGridSnapshot::session(
            self.session_grid_stamp.grid_id(),
            self.session_grid_stamp.revision(),
            self.session_epoch,
            self.anchor,
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use kithara_warp::{
        BeatGridId, BeatGridRevision, BeatGridStamp, SessionAnchor, SessionAxis, SessionBeat,
        SessionEpoch, SessionFrame,
    };

    use super::{SessionTransportSnapshot, Tempo, TempoError, TransportRevision};

    #[kithara::test]
    #[case::not_a_number(f64::NAN)]
    #[case::below_the_floor(0.5)]
    #[case::above_the_ceiling(1_000.5)]
    fn an_invalid_tempo_is_refused(#[case] beats_per_minute: f64) {
        assert!(matches!(
            Tempo::new(beats_per_minute),
            Err(TempoError { .. })
        ));
    }

    #[kithara::test]
    fn the_refusal_names_both_bounds() {
        let error = Tempo::new(0.5).expect_err("half a beat per minute is below the floor");
        let message = error.to_string();

        assert!(message.contains('1'), "the floor is named: {message}");
        assert!(message.contains("1000"), "the ceiling is named: {message}");
    }

    #[kithara::test]
    fn snapshot_carries_the_anchor_that_places_a_target_on_the_session_clock() {
        let anchor = SessionAnchor::new(
            SessionFrame::new(192_000),
            SessionBeat::new(8.0).expect("invariant: fixture beat is finite"),
            2.0,
            SessionAxis::new(
                NonZeroU32::new(48_000).expect("invariant: fixture rate is non-zero"),
                SessionEpoch::new(0),
            ),
        )
        .expect("invariant: fixture anchor is valid");
        let snapshot = SessionTransportSnapshot::new(
            SessionBeat::new(8.0).expect("invariant: fixture position is finite"),
            true,
            Tempo::new(120.0).expect("invariant: fixture tempo is in range"),
            TransportRevision::first(),
            anchor,
            BeatGridStamp::new(
                BeatGridId::allocate()
                    .expect("invariant: fixture grid identity space is available"),
                BeatGridRevision::first(),
            ),
            SessionEpoch::new(0),
        );
        let target = SessionBeat::new(11.0).expect("invariant: fixture target is finite");

        assert_eq!(
            snapshot
                .anchor()
                .frame_at(target)
                .expect("invariant: fixture target is representable"),
            SessionFrame::new(264_000)
        );
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
