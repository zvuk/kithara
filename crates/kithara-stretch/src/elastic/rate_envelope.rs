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
                let minimum = (output_frames * self.min_source_frames_per_output).floor();
                let maximum = (output_frames * self.max_source_frames_per_output).ceil();
                source_frames >= minimum && source_frames <= maximum
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
            && source_frames_per_output >= self.min_source_frames_per_output.next_down()
            && source_frames_per_output <= self.max_source_frames_per_output.next_up()
    }
}

#[cfg(all(test, feature = "stretch-signalsmith"))]
mod tests {
    use super::*;

    const OUTPUT_FRAMES: usize = 128;

    #[test]
    fn continuous_boundaries_accept_only_adjacent_rounding_error() {
        let envelope = ElasticRateEnvelope::signalsmith();
        let minimum = envelope.min_source_frames_per_output();
        let maximum = envelope.max_source_frames_per_output();

        assert!(envelope.contains_rate(minimum.next_down()));
        assert!(envelope.contains_rate(maximum.next_up()));
        assert!(!envelope.contains_rate(minimum.next_down().next_down()));
        assert!(!envelope.contains_rate(maximum.next_up().next_up()));
    }

    #[test]
    fn integer_requests_accept_floor_and_ceil_at_envelope_boundaries() {
        let envelope = ElasticRateEnvelope::signalsmith();

        for source_frames in [85, 86, 170, 171] {
            let request =
                ElasticRequest::new(source_frames, OUTPUT_FRAMES).expect("non-empty request");
            assert!(
                envelope.contains(request),
                "{source_frames}/{OUTPUT_FRAMES} must be an accepted boundary quantization"
            );
        }

        for source_frames in [84, 172] {
            let request =
                ElasticRequest::new(source_frames, OUTPUT_FRAMES).expect("non-empty request");
            assert!(
                !envelope.contains(request),
                "{source_frames}/{OUTPUT_FRAMES} exceeds one boundary quantization"
            );
        }
    }

    #[test]
    fn exact_integer_boundaries_do_not_gain_an_extra_frame() {
        let envelope = ElasticRateEnvelope::signalsmith();

        for source_frames in [2, 4] {
            let request = ElasticRequest::new(source_frames, 3).expect("non-empty request");
            assert!(envelope.contains(request));
        }

        for source_frames in [1, 5] {
            let request = ElasticRequest::new(source_frames, 3).expect("non-empty request");
            assert!(!envelope.contains(request));
        }
    }
}
