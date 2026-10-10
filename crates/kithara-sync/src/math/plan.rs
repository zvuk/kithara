use std::num::NonZeroU32;

use kithara_warp::{MIN_SPEED, SpeedCurve};
use num_traits::ToPrimitive;
/// One speed and its residence time in output seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CorrectionStep {
    pub speed: f32,
    pub seconds: f64,
}

/// A staircase whose integrated speed difference cancels the phase error.
#[derive(Clone, Debug, PartialEq)]
pub struct CorrectionPlan {
    pub steps: Vec<CorrectionStep>,
}

impl CorrectionPlan {
    /// The renderer curve that replaces the remaining correction in one command.
    /// Residence times are rounded on the output rate; every adjacent step keeps
    /// a distinct frame, including a zero-duration step between bounded speeds.
    ///
    /// # Panics
    /// Panics for a non-finite or unplayable speed, a non-finite or negative
    /// residence, or a cumulative duration or distinct frame offset that is
    /// not representable.
    #[must_use]
    pub fn curve(&self, sample_rate: NonZeroU32) -> SpeedCurve {
        match self.checked_curve(sample_rate) {
            Some((curve, _frames)) => curve,
            None => panic!("correction plan is not representable in output frames"),
        }
    }

    /// Returns the curve and its last frame offset, or `None` if unrepresentable.
    #[must_use]
    pub fn checked_curve(&self, sample_rate: NonZeroU32) -> Option<(SpeedCurve, u64)> {
        let steps = self.frame_steps(sample_rate).collect::<Option<Vec<_>>>()?;
        let frames = steps.last().map_or(0, |(frame, _speed)| *frame);
        Some((SpeedCurve::Steps(steps.into()), frames))
    }

    fn frame_steps(
        &self,
        sample_rate: NonZeroU32,
    ) -> impl Iterator<Item = Option<(u64, f32)>> + '_ {
        let mut seconds = 0.0;
        let mut previous: Option<u64> = None;
        self.steps.iter().map(move |step| {
            if !step.speed.is_finite()
                || step.speed < MIN_SPEED
                || !step.seconds.is_finite()
                || step.seconds < 0.0
            {
                return None;
            }
            let next_seconds = seconds + step.seconds;
            let duration = next_seconds * f64::from(sample_rate.get());
            let rounded = (seconds * f64::from(sample_rate.get())).round();
            let limit = u64::MAX.to_f64()?;
            if !duration.is_finite()
                || duration >= limit
                || !rounded.is_finite()
                || rounded < 0.0
                || rounded >= limit
            {
                return None;
            }
            let rounded = rounded.to_u64()?;
            let frame = match previous {
                Some(frame) => rounded.max(frame.checked_add(1)?),
                None => rounded,
            };
            previous = Some(frame);
            seconds = next_seconds;
            Some((frame, step.speed))
        })
    }
}
