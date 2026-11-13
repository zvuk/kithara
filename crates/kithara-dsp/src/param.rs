use std::num::NonZeroU32;

use firewheel_core::dsp::filter::smoothing_filter::MAX_SETTLE_RATIO;
pub use firewheel_core::{
    dsp::{
        filter::smoothing_filter::{
            DEFAULT_SETTLE_RATIO, DEFAULT_SMOOTH_SECONDS, MIN_SETTLE_RATIO, SmoothingFilter,
            SmoothingFilterCoeff,
        },
        mix::{Mix, MixDSP},
    },
    param::smoother::{DEFAULT_GAIN_SPAN, SmoothedParam, SmootherConfig},
};

/// Double-precision parameter state with the shared one-pole and settle law.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct SmoothedParam64 {
    target: f64,
    value: f64,
    target_times_a: f64,
    b1: f64,
    smooth_seconds: f64,
    settle_ratio: f64,
    span: f64,
}

impl SmoothedParam64 {
    /// Start at `initial_value`; timing and settlement use `SmootherConfig`.
    #[must_use]
    pub fn new(
        initial_value: f64,
        value_span: f64,
        config: SmootherConfig,
        sample_rate: NonZeroU32,
    ) -> Self {
        let smooth_seconds = f64::from(config.smooth_seconds.max(0.0));
        let settle_ratio = f64::from(
            config
                .settle_ratio
                .clamp(MIN_SETTLE_RATIO, MAX_SETTLE_RATIO),
        );
        let b1 = Self::decay(smooth_seconds, settle_ratio, sample_rate);
        Self {
            target: initial_value,
            value: initial_value,
            target_times_a: initial_value * (1.0 - b1),
            b1,
            smooth_seconds,
            settle_ratio,
            span: value_span.abs(),
        }
    }

    /// Replace the target without changing the current value.
    pub fn set_value(&mut self, value: f64) {
        self.target = value;
        self.target_times_a = value * (1.0 - self.b1);
    }

    /// Advance one frame using separate multiplies and addition.
    #[inline]
    pub fn next_smoothed(&mut self) -> f64 {
        self.value = self.target_times_a + self.value * self.b1;
        self.value
    }

    /// Snap within the configured fraction of the span; the boundary is strict.
    pub fn settle(&mut self) -> bool {
        if self.value == self.target {
            true
        } else if self.span == 0.0
            || (self.value - self.target).abs() < (self.span * self.settle_ratio).abs()
        {
            self.value = self.target;
            true
        } else {
            false
        }
    }

    /// Whether the retained value equals its current target.
    #[must_use]
    pub fn has_settled(&self) -> bool {
        self.value == self.target
    }

    /// The retained value, without advancing or settling the filter.
    #[must_use]
    pub fn current_value(&self) -> f64 {
        self.value
    }

    /// Clear smoothing history while retaining the target.
    pub fn reset_to_target(&mut self) {
        self.value = self.target;
    }

    /// Recompute time constants without resetting the current value.
    pub fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.b1 = Self::decay(self.smooth_seconds, self.settle_ratio, sample_rate);
        self.target_times_a = self.target * (1.0 - self.b1);
    }

    fn decay(seconds: f64, ratio: f64, sample_rate: NonZeroU32) -> f64 {
        let frames = (seconds * f64::from(sample_rate.get())).max(1.0);
        ratio.powf(frames.recip())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::{SmoothedParam64, SmootherConfig};

    fn half_life() -> SmootherConfig {
        SmootherConfig {
            smooth_seconds: 1.0,
            settle_ratio: 0.25,
        }
    }

    #[kithara::test]
    fn double_smoother_keeps_the_one_pole_law_and_strict_settle_boundary() {
        let rate = NonZeroU32::new(2).expect("two frames per second");
        let mut value = SmoothedParam64::new(200.0, 600.0, half_life(), rate);
        value.set_value(800.0);
        assert_eq!(value.next_smoothed(), 500.0);
        assert!(!value.settle());
        assert_eq!(value.next_smoothed(), 650.0);
        assert!(!value.settle());
        assert_eq!(value.next_smoothed(), 725.0);
        assert!(value.settle());
        assert!(value.has_settled());
        assert_eq!(value.next_smoothed(), 800.0);
    }

    #[kithara::test]
    fn double_smoother_does_not_round_targets_to_single_precision() {
        let rate = NonZeroU32::new(2).expect("two frames per second");
        let low = 800.0 - 1.0e-9;
        let high = 800.0 + 1.0e-9;
        let mut value = SmoothedParam64::new(low, 1.0e-8, half_life(), rate);
        value.set_value(high);
        assert_eq!(value.next_smoothed(), 800.0);
        assert!(!value.has_settled());
        value.reset_to_target();
        assert!(value.has_settled());
        assert_eq!(value.next_smoothed(), high);
    }

    #[kithara::test]
    fn double_smoother_rate_change_preserves_history_and_recomputes_decay() {
        let rate = NonZeroU32::new(2).expect("two frames per second");
        let mut value = SmoothedParam64::new(200.0, 600.0, half_life(), rate);
        value.set_value(800.0);
        assert_eq!(value.next_smoothed(), 500.0);
        value.update_sample_rate(NonZeroU32::MIN);
        assert_eq!(value.next_smoothed(), 725.0);
        assert!(value.settle());
    }

    #[kithara::test]
    fn double_smoother_is_independent_of_caller_partitioning() {
        let rate = NonZeroU32::new(48_000).expect("audio rate");
        let mut expected = SmoothedParam64::new(20.0, 19_980.0, SmootherConfig::default(), rate);
        expected.set_value(20_000.0);
        let mut actual = expected;
        let reference: Vec<_> = (0..4096)
            .map(|_| {
                let sample = expected.next_smoothed();
                expected.settle();
                sample.to_bits()
            })
            .collect();
        let mut output = vec![0_u64; reference.len()];
        for block in output.chunks_mut(7) {
            for sample in block {
                *sample = actual.next_smoothed().to_bits();
                actual.settle();
            }
        }
        assert_eq!(output, reference);
        assert_eq!(actual, expected);
    }
}
