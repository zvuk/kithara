use kithara_effects::filter::FilterKnob;
use kithara_signal::SessionFrame;

use super::{ParameterCurve, ParameterCurveError, ParameterKnot};

/// Prepared positions of the deck filter knob, one value per frame.
///
/// Values are knob positions in `[-1, 1]`; a linear segment moves the knob
/// linearly, so the cutoff follows the knob's geometric map.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct FilterProgram {
    curve: ParameterCurve,
}

impl FilterProgram {
    pub const MAX_KNOTS: usize = ParameterCurve::MAX_KNOTS;

    /// Prepare finite, ordered knob positions.
    /// # Errors
    /// Refuses invalid knots or positions outside the knob range.
    pub fn new(knots: Vec<ParameterKnot>) -> Result<Self, ParameterCurveError> {
        let curve = ParameterCurve::new(knots)?;
        let range = curve.value_range();
        if FilterKnob::checked(*range.start()).is_none()
            || FilterKnob::checked(*range.end()).is_none()
        {
            return Err(ParameterCurveError::Range);
        }
        Ok(Self { curve })
    }

    delegate::delegate! {
        to self.curve {
            /// First addressed session frame.
            #[must_use]
            pub fn start(&self) -> SessionFrame;
            /// Final addressed frame, whose position is held afterward.
            #[must_use]
            pub fn end(&self) -> SessionFrame;
            /// The knob position on frame `at`, which must not precede the start.
            #[must_use]
            pub(crate) fn value_at(&self, at: SessionFrame) -> f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_dsp::curve::CurveShape;
    use kithara_test_utils::kithara;

    use super::*;

    fn knot(at: i64, value: f64, shape: CurveShape) -> ParameterKnot {
        ParameterKnot::new(SessionFrame::new(at), value, shape).expect("finite knot")
    }

    #[kithara::test]
    fn positions_outside_the_knob_are_refused_and_its_ends_are_kept() {
        for value in [-1.000_000_1, 1.000_000_1, 2.0] {
            assert!(matches!(
                FilterProgram::new(vec![knot(10, value, CurveShape::HOLD)]),
                Err(ParameterCurveError::Range)
            ));
        }
        for value in [-1.0, 0.0, 1.0, 0.333_333_333_3] {
            let program =
                FilterProgram::new(vec![knot(10, value, CurveShape::HOLD)]).expect("program");
            assert_eq!(
                program.value_at(SessionFrame::new(10)).to_bits(),
                value.to_bits()
            );
        }
    }

    #[kithara::test]
    fn a_linear_segment_moves_the_knob_linearly_and_holds_its_end() {
        let program = FilterProgram::new(vec![
            knot(10, -0.5, CurveShape::LINEAR),
            knot(26, 0.5, CurveShape::HOLD),
        ])
        .expect("program");
        for (frame, value) in [(10, -0.5), (18, 0.0), (26, 0.5), (40, 0.5)] {
            assert_eq!(program.value_at(SessionFrame::new(frame)), value);
        }
    }
}
