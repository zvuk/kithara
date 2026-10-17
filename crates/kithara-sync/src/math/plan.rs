use std::num::NonZeroU32;

use num_traits::ToPrimitive;

use crate::MIN_SPEED;
/// One speed and its residence time in output seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CorrectionStep {
    pub speed: f64,
    pub seconds: f64,
}

/// Epsilon-bounded media-speed steps converging to a local reference speed.
#[derive(Clone, Debug, PartialEq)]
pub struct CorrectionPlan {
    pub steps: Vec<CorrectionStep>,
}

impl CorrectionPlan {
    pub(super) fn media_offsets(
        &self,
        sample_rate: NonZeroU32,
        reference: f64,
    ) -> Option<Vec<(u64, f64)>> {
        let steps = self.frame_steps(sample_rate).collect::<Option<Vec<_>>>()?;
        steps.last()?;
        Some(
            steps
                .into_iter()
                .map(|(frame, speed)| (frame, speed - reference))
                .collect(),
        )
    }

    fn frame_steps(
        &self,
        sample_rate: NonZeroU32,
    ) -> impl Iterator<Item = Option<(u64, f64)>> + '_ {
        let mut seconds = 0.0;
        let mut previous: Option<u64> = None;
        self.steps.iter().map(move |step| {
            if !step.speed.is_finite()
                || step.speed < f64::from(MIN_SPEED)
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
