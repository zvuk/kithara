#[cfg(feature = "stretch-signalsmith")]
use num_traits::ToPrimitive;

#[cfg(feature = "stretch-signalsmith")]
use super::ElasticRequest;

/// Supported source-frame advance per output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticRateEnvelope {
    min_source_frames_per_output: f64,
    max_source_frames_per_output: f64,
}

impl ElasticRateEnvelope {
    #[cfg(feature = "stretch-signalsmith")]
    const SIGNALSMITH_MIN_SOURCE_FRAMES_PER_OUTPUT: f64 = 2.0 / 3.0;
    #[cfg(feature = "stretch-signalsmith")]
    const SIGNALSMITH_MAX_SOURCE_FRAMES_PER_OUTPUT: f64 = 4.0 / 3.0;

    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) const fn signalsmith() -> Self {
        Self {
            min_source_frames_per_output: Self::SIGNALSMITH_MIN_SOURCE_FRAMES_PER_OUTPUT,
            max_source_frames_per_output: Self::SIGNALSMITH_MAX_SOURCE_FRAMES_PER_OUTPUT,
        }
    }

    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) fn contains(self, request: ElasticRequest) -> bool {
        request
            .source_frames()
            .to_f64()
            .zip(request.output_frames().to_f64())
            .is_some_and(|(source_frames, output_frames)| {
                let rate = source_frames / output_frames;
                rate >= self.min_source_frames_per_output
                    && rate <= self.max_source_frames_per_output
            })
    }

    /// Minimum source-frame advance per output frame.
    #[must_use]
    pub const fn min_source_frames_per_output(self) -> f64 {
        self.min_source_frames_per_output
    }

    /// Maximum source-frame advance per output frame.
    #[must_use]
    pub const fn max_source_frames_per_output(self) -> f64 {
        self.max_source_frames_per_output
    }

    /// Returns whether a continuous source advance is supported.
    #[must_use]
    pub fn contains_rate(self, source_frames_per_output: f64) -> bool {
        source_frames_per_output.is_finite()
            && source_frames_per_output >= self.min_source_frames_per_output
            && source_frames_per_output <= self.max_source_frames_per_output
    }
}
