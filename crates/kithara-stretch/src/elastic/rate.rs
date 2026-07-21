#[cfg(feature = "stretch-signalsmith")]
use num_traits::ToPrimitive;

#[cfg(feature = "stretch-signalsmith")]
use super::ElasticRequest;

/// Supported source-frame advance per output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticRateEnvelope {
    pub(super) max_source_frames_per_output: f64,
    pub(super) min_source_frames_per_output: f64,
}

impl ElasticRateEnvelope {
    #[cfg(feature = "stretch-signalsmith")]
    const SIGNALSMITH_MAX_SOURCE_FRAMES_PER_OUTPUT: f64 = 4.0 / 3.0;
    #[cfg(feature = "stretch-signalsmith")]
    const SIGNALSMITH_MIN_SOURCE_FRAMES_PER_OUTPUT: f64 = 2.0 / 3.0;

    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) fn contains(self, request: ElasticRequest) -> bool {
        request
            .source_frames()
            .to_f64()
            .zip(request.output_frames().to_f64())
            .is_some_and(|(source_frames, output_frames)| {
                self.contains_rate(source_frames / output_frames)
            })
    }

    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) const fn signalsmith() -> Self {
        Self {
            max_source_frames_per_output: Self::SIGNALSMITH_MAX_SOURCE_FRAMES_PER_OUTPUT,
            min_source_frames_per_output: Self::SIGNALSMITH_MIN_SOURCE_FRAMES_PER_OUTPUT,
        }
    }

    /// Returns whether a continuous source advance is supported.
    #[must_use]
    pub fn contains_rate(self, source_frames_per_output: f64) -> bool {
        source_frames_per_output.is_finite()
            && source_frames_per_output >= self.min_source_frames_per_output.next_down()
            && source_frames_per_output <= self.max_source_frames_per_output.next_up()
    }

    /// Maximum source-frame advance per output frame.
    #[must_use]
    pub const fn max_source_frames_per_output(self) -> f64 {
        self.max_source_frames_per_output
    }

    /// Minimum source-frame advance per output frame.
    #[must_use]
    pub const fn min_source_frames_per_output(self) -> f64 {
        self.min_source_frames_per_output
    }
}

#[cfg(all(test, feature = "stretch-signalsmith"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::ElasticRateEnvelope;

    #[kithara::test]
    fn accepts_one_rounding_step_at_the_declared_rate_boundary() {
        let envelope = ElasticRateEnvelope::signalsmith();
        let minimum = envelope.min_source_frames_per_output();
        let maximum = envelope.max_source_frames_per_output();
        let one_step_below = minimum.next_down();
        let two_steps_below = one_step_below.next_down();
        let one_step_above = maximum.next_up();
        let two_steps_above = one_step_above.next_up();

        assert!(envelope.contains_rate(one_step_below));
        assert!(!envelope.contains_rate(two_steps_below));
        assert!(envelope.contains_rate(one_step_above));
        assert!(!envelope.contains_rate(two_steps_above));
    }
}
