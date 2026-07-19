/// Supported source-frame advance per output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticRateEnvelope {
    pub(super) max_source_frames_per_output: f64,
    pub(super) min_source_frames_per_output: f64,
}

impl ElasticRateEnvelope {
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
        let one_step_above = f64::from_bits(maximum.to_bits() + 1);
        let two_steps_above = f64::from_bits(maximum.to_bits() + 2);

        assert!(envelope.contains_rate(one_step_below));
        assert!(!envelope.contains_rate(two_steps_below));
        assert!(envelope.contains_rate(one_step_above));
        assert!(!envelope.contains_rate(two_steps_above));
    }
}
