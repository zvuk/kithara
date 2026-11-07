use std::{num::NonZeroU32, ops::RangeInclusive};

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
use num_traits::ToPrimitive;

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

    /// Bound values and changing frames when each step is followed by `settle`.
    /// Returns `None` when a finite bound cannot be represented.
    #[must_use]
    pub fn motion_bound(&self) -> Option<(RangeInclusive<f64>, u64)> {
        if ![
            self.value,
            self.target,
            self.span,
            self.b1,
            self.target_times_a,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return None;
        }
        if self.has_settled() {
            return Some((self.value..=self.value, 0));
        }
        let low = self.value.min(self.target);
        let high = self.value.max(self.target);
        let distance = (self.value - self.target).abs();
        let threshold = self.span * self.settle_ratio;
        if self.span == 0.0 || self.b1 == 0.0 || (self.b1 == 1.0 && distance < threshold) {
            return Some((low..=high, 1));
        }
        if self.b1 == 1.0 {
            return Some((self.value..=self.value, 0));
        }
        if !(0.0..1.0).contains(&self.b1) {
            return None;
        }
        self.rounded_motion_bound(low..=high)
    }

    fn rounded_motion_bound(
        &self,
        bounds: RangeInclusive<f64>,
    ) -> Option<(RangeInclusive<f64>, u64)> {
        let distance = (self.value - self.target).abs();
        let threshold = self.span * self.settle_ratio;
        let magnitude = self
            .value
            .abs()
            .max(self.target.abs())
            .max(f64::MIN_POSITIVE);
        let floor = 8.0 * f64::EPSILON * magnitude / (1.0 - self.b1);
        if !floor.is_finite() || floor >= magnitude {
            return None;
        }
        let goal = if threshold > 2.0 * floor {
            threshold - floor
        } else {
            floor
        };
        let approach = decay_frames(self.b1, distance, goal)?;
        let lower = (self.target - 2.0 * floor).next_down();
        let upper = (self.target + 2.0 * floor).next_up();
        if !lower.is_finite() || !upper.is_finite() {
            return None;
        }
        let settling = if threshold > 2.0 * floor {
            0
        } else {
            ordered_bits(upper)
                .checked_sub(ordered_bits(lower))?
                .checked_add(1)?
        };
        Some((
            (*bounds.start()).min(lower)..=(*bounds.end()).max(upper),
            approach.checked_add(settling)?,
        ))
    }

    fn decay(seconds: f64, ratio: f64, sample_rate: NonZeroU32) -> f64 {
        let frames = (seconds * f64::from(sample_rate.get())).max(1.0);
        ratio.powf(frames.recip())
    }
}

fn decay_frames(decay: f64, distance: f64, goal: f64) -> Option<u64> {
    if distance <= goal {
        return Some(1);
    }
    let frames = ((goal / distance).ln() / decay.ln()).ceil();
    frames.to_u64()?.checked_add(2)
}

fn ordered_bits(value: f64) -> u64 {
    let bits = value.to_bits();
    if value.is_sign_negative() {
        !bits
    } else {
        bits | (1_u64 << 63)
    }
}

/// Clock-free progression for a prepared parameter segment.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct CurveShape(Progression);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Progression {
    Hold,
    Linear,
    Cubic { first: f64, second: f64 },
}

/// Cubic progression handles must be finite, ordered and inside zero to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("cubic progression handles must satisfy 0 <= first <= second <= 1")]
pub struct CurveShapeError;

impl CurveShape {
    pub const HOLD: Self = Self(Progression::Hold);
    pub const LINEAR: Self = Self(Progression::Linear);

    /// Prepare a monotone cubic Bezier with time handles at 1/3 and 2/3.
    /// # Errors
    /// Refuses nonfinite, unordered or out-of-range progression handles.
    pub fn cubic(first: f64, second: f64) -> Result<Self, CurveShapeError> {
        if first.is_finite()
            && second.is_finite()
            && 0.0 <= first
            && first <= second
            && second <= 1.0
        {
            Ok(Self(Progression::Cubic { first, second }))
        } else {
            Err(CurveShapeError)
        }
    }

    /// Evaluate normalized time in f64; exact endpoints are copied directly.
    #[must_use]
    pub fn progress(self, time: f64) -> f64 {
        if time <= 0.0 {
            return 0.0;
        }
        if time >= 1.0 {
            return 1.0;
        }
        match self.0 {
            Progression::Hold => 0.0,
            Progression::Linear => time,
            Progression::Cubic { first, second } => {
                let remaining = 1.0 - time;
                let left = 3.0 * remaining * remaining * time * first;
                let right = 3.0 * remaining * time * time * second;
                ((left + right) + time * time * time).clamp(0.0, 1.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::{CurveShape, SmoothedParam64, SmootherConfig};

    #[kithara::test]
    fn cubic_handles_are_validated_before_a_shape_exists() {
        for (first, second) in [
            (-0.1, 0.5),
            (0.5, 1.1),
            (0.8, 0.2),
            (f64::NAN, 0.5),
            (0.5, f64::NAN),
            (f64::INFINITY, 1.0),
        ] {
            assert!(CurveShape::cubic(first, second).is_err());
        }
    }

    #[kithara::test]
    fn cubic_quarter_points_match_independent_polynomial_values() {
        for (handles, expected) in [
            ((0.0, 0.0), [0.0, 0.015625, 0.125, 0.421875, 1.0]),
            ((1.0, 1.0), [0.0, 0.578125, 0.875, 0.984375, 1.0]),
            ((0.0, 1.0), [0.0, 0.15625, 0.5, 0.84375, 1.0]),
        ] {
            let shape = CurveShape::cubic(handles.0, handles.1).expect("bounded handles");
            for (time, expected) in [0.0, 0.25, 0.5, 0.75, 1.0].into_iter().zip(expected) {
                assert_eq!(shape.progress(time), expected);
            }
        }
        assert_eq!(CurveShape::HOLD.progress(0.5), 0.0);
        assert_eq!(CurveShape::LINEAR.progress(0.5), 0.5);
    }

    #[kithara::test]
    fn progression_is_monotone_and_bounded_including_exact_endpoints() {
        for shape in [
            CurveShape::HOLD,
            CurveShape::LINEAR,
            CurveShape::cubic(0.0, 0.0).expect("ease in"),
            CurveShape::cubic(1.0, 1.0).expect("ease out"),
            CurveShape::cubic(0.0, 1.0).expect("S shape"),
        ] {
            let mut previous = 0.0;
            for frame in 0..=1024_u16 {
                let value = shape.progress(f64::from(frame) / 1024.0);
                assert!((previous..=1.0).contains(&value));
                previous = value;
            }
            assert_eq!(shape.progress(0.0).to_bits(), 0.0_f64.to_bits());
            assert_eq!(shape.progress(1.0).to_bits(), 1.0_f64.to_bits());
        }
    }

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

    #[kithara::test]
    fn motion_bound_covers_settlement_and_a_rounded_fixed_point() {
        let rate = NonZeroU32::new(44_100).expect("rate");
        for (from, to) in [(1_000.0, 200.0), (20.0, 20.0_f64.next_up()), (-24.0, 0.0)] {
            let mut value =
                SmoothedParam64::new(from, (to - from).abs(), SmootherConfig::default(), rate);
            value.set_value(to);
            let (range, frames) = value.motion_bound().expect("finite changing-frame bound");
            let mut stopped = false;
            for _ in 0..frames {
                let before = value.current_value();
                value.next_smoothed();
                value.settle();
                assert!(range.contains(&value.current_value()));
                if value.has_settled() || before.to_bits() == value.current_value().to_bits() {
                    stopped = true;
                    break;
                }
            }
            assert!(stopped, "motion exceeded its bound: {from} -> {to}");
        }
    }
}
