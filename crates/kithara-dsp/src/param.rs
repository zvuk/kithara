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
    use kithara_test_utils::kithara;

    use super::CurveShape;

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
}
