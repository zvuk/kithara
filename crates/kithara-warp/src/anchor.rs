use std::num::NonZeroU32;

use num_traits::cast::ToPrimitive;

use crate::SessionAxis;

/// An invalid session coordinate or coordinate rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum CoordinateError {
    /// The supplied coordinate is `NaN` or infinite.
    #[error("coordinate must be finite")]
    NonFinite,
    /// A coordinate rate cannot define an invertible frame relation.
    #[error("coordinate rate must advance by a finite, positive amount per frame")]
    NonInvertibleRate,
}

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
}

/// The canonical session-beat <-> session-frame relation, valid from one
/// committed render frame. The transport replaces it on every tempo commit.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SessionAnchor {
    /// Returns the session coordinate system this anchor counts frames in.
    ///
    /// The axis is the session's own rate and epoch, so it travels from the
    /// owner that chose it. A deck holds a replica of the axis and must not
    /// reconstruct it, because a rate only changes across an epoch boundary
    /// the owner declares.
    #[field(get, copy)]
    axis: SessionAxis,
    /// Returns the session beat playing at [`Self::frame`].
    #[field(get, copy)]
    beat: SessionBeat,
    /// Returns the frame this anchor was established on.
    #[field(get, copy)]
    frame: SessionFrame,
    /// Returns the committed tempo in beats per second at [`Self::frame`].
    #[field(get, copy)]
    beats_per_second: f64,
    /// Returns the tempo this anchor approaches in beats per second.
    ///
    /// It differs from [`Self::beats_per_second`] only while a tempo change is
    /// still being approached; a settled anchor holds one tempo.
    #[field(get, copy)]
    target_beats_per_second: f64,
    /// Returns the time constant of the approach in seconds.
    #[field(get, copy)]
    smooth_seconds: f64,
}

impl SessionAnchor {
    /// Newton steps that invert the beat advance of an approach.
    const INVERSION_STEPS: usize = 32;

    /// Seconds below which an inversion step no longer moves the frame.
    const INVERSION_EPSILON: f64 = 1e-15;

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
        axis: SessionAxis,
    ) -> Result<Self, CoordinateError> {
        let beats_per_frame = beats_per_second / f64::from(axis.sample_rate().get());
        if !beats_per_second.is_finite()
            || beats_per_second <= 0.0
            || !beats_per_frame.is_finite()
            || beats_per_frame <= 0.0
        {
            return Err(CoordinateError::NonInvertibleRate);
        }
        Ok(Self {
            axis,
            beat,
            frame,
            beats_per_second,
            target_beats_per_second: beats_per_second,
            smooth_seconds: 0.0,
        })
    }

    /// Approaches `beats_per_second` from the tempo playing at `frame`, over a
    /// time constant of `smooth_seconds`.
    ///
    /// The beat and the tempo at `frame` are the ones this anchor already
    /// plays, so a target that arrives while an earlier one is still being
    /// approached neither steps the music nor is refused.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the target is not an invertible rate or
    /// the beat at `frame` is not representable.
    pub fn retarget(
        self,
        frame: SessionFrame,
        beats_per_second: f64,
        smooth_seconds: f64,
    ) -> Result<Self, CoordinateError> {
        let reached = self.tempo_at(frame);
        let mut next = Self::new(frame, self.beat_at(frame)?, reached, self.axis)?;
        if !beats_per_second.is_finite()
            || beats_per_second <= 0.0
            || !smooth_seconds.is_finite()
            || smooth_seconds < 0.0
        {
            return Err(CoordinateError::NonInvertibleRate);
        }
        if smooth_seconds > 0.0 {
            next.target_beats_per_second = beats_per_second;
            next.smooth_seconds = smooth_seconds;
        } else {
            next = Self::new(frame, next.beat, beats_per_second, self.axis)?;
        }
        Ok(next)
    }

    /// The tempo playing at `frame`, in beats per second.
    #[must_use]
    pub fn tempo_at(self, frame: SessionFrame) -> f64 {
        let Some(elapsed) = self.elapsed_seconds(frame) else {
            return self.beats_per_second;
        };
        self.target_beats_per_second
            + (self.beats_per_second - self.target_beats_per_second)
                * (-elapsed / self.smooth_seconds).exp()
    }

    /// Whether a target tempo is still being approached from this anchor.
    fn approaching(self) -> bool {
        self.smooth_seconds > 0.0 && self.target_beats_per_second != self.beats_per_second
    }

    /// Seconds from this anchor to a later `frame` while a target is still
    /// approached; frames before the anchor play its starting tempo.
    fn elapsed_seconds(self, frame: SessionFrame) -> Option<f64> {
        if !self.approaching() {
            return None;
        }
        let frames = i64::from(frame)
            .checked_sub(i64::from(self.frame))
            .filter(|value| *value > 0)
            .and_then(|value| value.to_f64())?;
        Some(frames / f64::from(self.axis.sample_rate().get()))
    }

    /// Inverts [`Self::advanced_beats`] by Newton steps on the approach.
    ///
    /// The beat advance is strictly increasing in time because every tempo on
    /// the approach is positive, so the root is unique and the steps converge.
    fn ramped_frame_at(self, beats: f64) -> Result<SessionFrame, CoordinateError> {
        if !beats.is_finite() {
            return Err(CoordinateError::NonFinite);
        }
        let rate = f64::from(self.axis.sample_rate().get());
        let mut elapsed = beats / self.target_beats_per_second;
        for _ in 0..Self::INVERSION_STEPS {
            let tempo = self.target_beats_per_second
                + (self.beats_per_second - self.target_beats_per_second)
                    * (-elapsed / self.smooth_seconds).exp();
            let step = (self.advanced_beats(elapsed) - beats) / tempo;
            elapsed -= step;
            if step.abs() < Self::INVERSION_EPSILON {
                break;
            }
        }
        (elapsed * rate)
            .round()
            .to_i64()
            .and_then(|frames| i64::from(self.frame).checked_add(frames))
            .map(SessionFrame::new)
            .ok_or(CoordinateError::NonFinite)
    }

    /// Beats advanced from this anchor over `elapsed` seconds of approach.
    fn advanced_beats(self, elapsed: f64) -> f64 {
        let approach = self.smooth_seconds * (1.0 - (-elapsed / self.smooth_seconds).exp());
        self.target_beats_per_second * elapsed
            + (self.beats_per_second - self.target_beats_per_second) * approach
    }

    /// The session beat playing at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the frame is so far from the anchor
    /// that the beat is not representable.
    pub fn beat_at(self, frame: SessionFrame) -> Result<SessionBeat, CoordinateError> {
        if let Some(elapsed) = self.elapsed_seconds(frame) {
            return SessionBeat::new(f64::from(self.beat) + self.advanced_beats(elapsed));
        }
        let frames = i64::from(frame)
            .checked_sub(i64::from(self.frame))
            .and_then(|value| value.to_f64())
            .ok_or(CoordinateError::NonFinite)?;
        SessionBeat::new(f64::from(self.beat) + frames * self.beats_per_frame())
    }

    /// The session sample rate this anchor counts frames in.
    #[must_use]
    pub fn sample_rate(self) -> NonZeroU32 {
        self.axis.sample_rate()
    }

    /// Session beats one output frame advances at this tempo.
    #[must_use]
    pub fn beats_per_frame(self) -> f64 {
        self.beats_per_second / f64::from(self.axis.sample_rate().get())
    }

    /// Inverse of [`Self::beat_at`], rounded to the nearest frame.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when the frame is not representable.
    pub fn frame_at(self, beat: SessionBeat) -> Result<SessionFrame, CoordinateError> {
        let beats = f64::from(beat) - f64::from(self.beat);
        if self.approaching() && beats > 0.0 {
            return self.ramped_frame_at(beats);
        }
        let frames = (beats / self.beats_per_frame())
            .round()
            .to_i64()
            .ok_or(CoordinateError::NonFinite)?;
        i64::from(self.frame)
            .checked_add(frames)
            .map(SessionFrame::new)
            .ok_or(CoordinateError::NonFinite)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::{CoordinateError, SessionAnchor, SessionAxis, SessionBeat, SessionFrame};
    use crate::SessionEpoch;

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
            SessionAxis::new(rate(), SessionEpoch::new(0)),
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

    const SMOOTH_SECONDS: f64 = 0.005;

    fn frame(value: i64) -> SessionFrame {
        SessionFrame::new(value)
    }

    fn beat_value(anchor: SessionAnchor, at: i64) -> f64 {
        f64::from(
            anchor
                .beat_at(frame(at))
                .expect("invariant: the fixture beat is representable"),
        )
    }

    #[kithara::test]
    fn a_retarget_keeps_the_beat_and_the_tempo_continuous() {
        let ramp = anchor_at(0, 0.0)
            .retarget(frame(0), 3.0, SMOOTH_SECONDS)
            .expect("invariant: the first target is a positive rate");
        let before = (beat_value(ramp, 100), ramp.tempo_at(frame(100)));

        let retargeted = ramp
            .retarget(frame(100), 1.5, SMOOTH_SECONDS)
            .expect("invariant: the second target is a positive rate");

        assert_eq!(beat_value(retargeted, 100), before.0);
        assert_eq!(retargeted.tempo_at(frame(100)), before.1);
        assert!(
            before.1 > Consts::BEATS_PER_SECOND && before.1 < 3.0,
            "a retarget mid-ramp starts from the smoothed tempo, got {}",
            before.1
        );
    }

    #[kithara::test]
    fn frames_before_a_retarget_play_the_tempo_it_started_from() {
        let ramp = anchor_at(48_000, 4.0)
            .retarget(frame(48_000), 3.0, SMOOTH_SECONDS)
            .expect("invariant: the target is a positive rate");

        assert_eq!(ramp.tempo_at(frame(0)), Consts::BEATS_PER_SECOND);
        assert_eq!(beat_value(ramp, 0), 2.0);
        assert_eq!(
            ramp.frame_at(SessionBeat::new(2.0).expect("invariant: a finite beat")),
            Ok(frame(0))
        );
    }

    #[kithara::test]
    fn a_ramp_beat_is_the_integral_of_its_tempo() {
        let ramp = anchor_at(0, 4.0)
            .retarget(frame(0), 3.0, SMOOTH_SECONDS)
            .expect("invariant: the target is a positive rate");
        let rate = f64::from(Consts::RATE);
        let frames = 2_048_i64;
        let integrated = (0..frames)
            .map(|at| (ramp.tempo_at(frame(at)) + ramp.tempo_at(frame(at + 1))) / (2.0 * rate))
            .sum::<f64>();

        let advanced = beat_value(ramp, frames) - beat_value(ramp, 0);

        // The bound is the trapezoid error on the exponential, not a product
        // tolerance: 1e-7 beat is five microns of a frame at this tempo.
        assert!(
            (advanced - integrated).abs() < 1e-7,
            "beat advance {advanced} must equal the integrated tempo {integrated}"
        );
    }

    #[kithara::test]
    fn frame_at_inverts_beat_at_on_a_ramp() {
        let ramp = anchor_at(0, 0.0)
            .retarget(frame(0), 2.5, SMOOTH_SECONDS)
            .expect("invariant: the target is a positive rate");

        for at in [0, 1, 17, 128, 240, 1_000, 48_000] {
            let round_tripped = ramp
                .beat_at(frame(at))
                .and_then(|beat| ramp.frame_at(beat))
                .expect("invariant: the round trip stays representable");
            assert_eq!(round_tripped, frame(at), "frame {at}");
        }
    }

    #[kithara::test]
    fn a_settled_ramp_advances_at_its_target_tempo() {
        let ramp = anchor_at(0, 0.0)
            .retarget(frame(0), 3.0, SMOOTH_SECONDS)
            .expect("invariant: the target is a positive rate");
        let settled = Consts::FRAMES_PER_BEAT * 4;

        let advanced =
            beat_value(ramp, settled + Consts::FRAMES_PER_BEAT) - beat_value(ramp, settled);

        assert!(
            (advanced - 1.5).abs() < 1e-9,
            "half a second at 3 beats per second advances 1.5 beats, got {advanced}"
        );
    }

    #[kithara::test]
    fn every_retarget_of_a_turning_knob_is_accepted() {
        let mut ramp = anchor_at(0, 0.0);
        for step in 0..64_i64 {
            let target = if step % 2 == 0 { 2.1 } else { 1.9 };
            ramp = ramp
                .retarget(frame(step * 128), target, SMOOTH_SECONDS)
                .expect("invariant: every knob position is a positive rate");
            let tempo = ramp.tempo_at(frame(step * 128));
            assert!(
                (1.9..=Consts::BEATS_PER_SECOND.max(2.1)).contains(&tempo),
                "step {step}: tempo {tempo} left the knob range"
            );
        }
    }

    #[kithara::test]
    fn a_tempo_that_is_not_a_positive_rate_is_refused() {
        for refused in [0.0, -2.0, f64::from_bits(1), f64::NAN, f64::INFINITY] {
            assert_eq!(
                SessionAnchor::new(
                    SessionFrame::new(0),
                    beat(0.0),
                    refused,
                    SessionAxis::new(rate(), SessionEpoch::new(0)),
                ),
                Err(CoordinateError::NonInvertibleRate),
                "tempo {refused} is not an invertible slope",
            );
        }
    }
}
