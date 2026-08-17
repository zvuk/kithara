use std::num::NonZeroU32;

use num_traits::cast::ToPrimitive;

use super::CoordinateError;

/// A continuous beat coordinate on the session transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct SessionBeat(f64);

impl SessionBeat {
    /// Creates a finite session-beat coordinate. Negative beats are valid.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when `value` is not finite.
    pub const fn new(value: f64) -> Result<Self, CoordinateError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(CoordinateError::NonFinite)
        }
    }
}

impl TryFrom<f64> for SessionBeat {
    type Error = CoordinateError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A frame on the session clock, counted from the master ring's origin.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Into)]
#[repr(transparent)]
pub struct SessionFrame(i64);

impl SessionFrame {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns this session coordinate advanced by `frames`.
    #[must_use]
    pub fn offset(self, frames: u64) -> Option<Self> {
        let frames = i64::try_from(frames).ok()?;
        i64::from(self).checked_add(frames).map(Self::new)
    }
}

/// The canonical session-beat <-> session-frame relation, valid from one
/// committed render frame. The transport replaces it on every tempo commit.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SessionAnchor {
    /// Returns the frame this anchor was established on.
    #[field(get, copy)]
    frame: SessionFrame,
    /// Returns the session beat playing at [`Self::frame`].
    #[field(get, copy)]
    beat: SessionBeat,
    /// Returns the committed tempo in beats per second.
    #[field(get, copy)]
    beats_per_second: f64,
    /// Returns the session sample rate this anchor counts frames in.
    #[field(get, copy)]
    sample_rate: NonZeroU32,
}

impl SessionAnchor {
    /// Pins `beat` to `frame` at `beats_per_second`.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError::NonInvertibleRate`] unless the tempo advances
    /// by a finite, positive amount per output frame.
    pub fn new(
        frame: SessionFrame,
        beat: SessionBeat,
        beats_per_second: f64,
        sample_rate: NonZeroU32,
    ) -> Result<Self, CoordinateError> {
        let beats_per_frame = beats_per_second / f64::from(sample_rate.get());
        if !beats_per_second.is_finite()
            || beats_per_second <= 0.0
            || !beats_per_frame.is_finite()
            || beats_per_frame <= 0.0
        {
            return Err(CoordinateError::NonInvertibleRate);
        }
        Ok(Self {
            frame,
            beat,
            beats_per_second,
            sample_rate,
        })
    }

    /// The session beat playing at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the frame is so far from the anchor
    /// that the beat is not representable.
    pub fn beat_at(self, frame: SessionFrame) -> Result<SessionBeat, CoordinateError> {
        let frames = i64::from(frame)
            .checked_sub(i64::from(self.frame))
            .and_then(|value| value.to_f64())
            .ok_or(CoordinateError::NonFinite)?;
        SessionBeat::new(f64::from(self.beat) + frames * self.beats_per_frame())
    }

    /// Inverse of [`Self::beat_at`], rounded to the nearest frame.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the frame is not representable.
    pub fn frame_at(self, beat: SessionBeat) -> Result<SessionFrame, CoordinateError> {
        let beats = f64::from(beat) - f64::from(self.beat);
        let frames = (beats / self.beats_per_frame())
            .round()
            .to_i64()
            .ok_or(CoordinateError::NonFinite)?;
        i64::from(self.frame)
            .checked_add(frames)
            .map(SessionFrame::new)
            .ok_or(CoordinateError::NonFinite)
    }

    /// Session beats one output frame advances at this tempo.
    #[must_use]
    pub fn beats_per_frame(self) -> f64 {
        self.beats_per_second / f64::from(self.sample_rate.get())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::{CoordinateError, SessionAnchor, SessionBeat, SessionFrame};

    struct Consts;

    impl Consts {
        const BEATS_PER_SECOND: f64 = 2.0;
        const FRAMES_PER_BEAT: i64 = 24_000;
        const RATE: u32 = 48_000;
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::RATE).expect("invariant: the fixture rate is non-zero")
    }

    fn beat(value: f64) -> SessionBeat {
        SessionBeat::new(value).expect("invariant: the fixture beat is finite")
    }

    fn anchor_at(frame: i64, at_beat: f64) -> SessionAnchor {
        SessionAnchor::new(
            SessionFrame::new(frame),
            beat(at_beat),
            Consts::BEATS_PER_SECOND,
            rate(),
        )
        .expect("invariant: the fixture tempo is a positive rate")
    }

    #[kithara::test]
    fn frame_at_inverts_beat_at_exactly() {
        let anchor = anchor_at(1_024, 2.5);
        let frame = SessionFrame::new(1_024 + Consts::FRAMES_PER_BEAT * 3);

        let round_tripped = anchor
            .beat_at(frame)
            .and_then(|beat| anchor.frame_at(beat))
            .expect("invariant: the round trip stays representable");

        assert_eq!(round_tripped, frame);
    }

    #[kithara::test]
    fn a_tempo_that_is_not_a_positive_rate_is_refused() {
        for refused in [0.0, -2.0, f64::from_bits(1), f64::NAN, f64::INFINITY] {
            assert_eq!(
                SessionAnchor::new(SessionFrame::new(0), beat(0.0), refused, rate()),
                Err(CoordinateError::NonInvertibleRate),
                "tempo {refused} is not an invertible slope",
            );
        }
    }

    #[kithara::test]
    fn session_frame_offset_is_checked() {
        assert_eq!(SessionFrame::new(41).offset(1), Some(SessionFrame::new(42)));
        assert_eq!(SessionFrame::new(i64::MAX).offset(1), None);
        assert_eq!(SessionFrame::new(0).offset(u64::MAX), None);
    }
}
