use kithara_effects::filter::{FilterParameter, FilterSettingsChange};
use kithara_signal::SessionFrame;

use super::{ParameterCurve, ParameterCurveError, ParameterKnot};

/// One immutable trajectory of an installed filter parameter.
#[derive(Clone, Debug)]
pub struct FilterProgram {
    parameter: FilterParameter,
    curve: ParameterCurve,
}

impl FilterProgram {
    pub const MAX_KNOTS: usize = ParameterCurve::MAX_KNOTS;

    /// Prepare finite, ordered values in the parameter's own units.
    /// # Errors
    /// Refuses invalid knots or values outside the canonical parameter range.
    pub fn new(
        parameter: FilterParameter,
        knots: Vec<ParameterKnot>,
    ) -> Result<Self, ParameterCurveError> {
        let curve = ParameterCurve::new(knots)?;
        let range = curve.value_range();
        if parameter.change(*range.start()).is_none() || parameter.change(*range.end()).is_none() {
            return Err(ParameterCurveError::Range);
        }
        Ok(Self { parameter, curve })
    }

    #[must_use]
    pub const fn parameter(&self) -> FilterParameter {
        self.parameter
    }

    pub(crate) fn curve(&self) -> &ParameterCurve {
        &self.curve
    }

    delegate::delegate! {
        to self.curve {
            #[must_use]
            pub fn start(&self) -> SessionFrame;
            #[must_use]
            pub fn end(&self) -> SessionFrame;
        }
    }

    pub(crate) fn change_at(&self, at: SessionFrame) -> FilterSettingsChange {
        let elapsed = at
            .frames_since(self.start())
            .expect("admitted program start");
        let (left, right, progress) = self.curve.segment_at(elapsed);
        let value = if left == right || progress == 0.0 {
            left
        } else if progress == 1.0 {
            right
        } else if self.parameter == FilterParameter::CutoffHz {
            left * (right / left).powf(progress)
        } else {
            left + (right - left) * progress
        };
        self.parameter
            .change(value)
            .expect("bounded monotone parameter trajectory")
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::bridge::CurveShape;

    #[kithara::test]
    fn fields_admit_their_own_ranges_without_narrowing_cutoff_or_q() {
        for (field, values) in [
            (FilterParameter::CutoffHz, [19.0, 20_001.0]),
            (FilterParameter::Q, [0.09, 10.01]),
            (FilterParameter::GainDb, [-24.01, 6.01]),
        ] {
            for value in values {
                let knot = ParameterKnot::new(SessionFrame::new(10), value, CurveShape::HOLD)
                    .expect("finite point");
                assert!(matches!(
                    FilterProgram::new(field, vec![knot]),
                    Err(ParameterCurveError::Range)
                ));
            }
        }
        for (field, value) in [
            (FilterParameter::CutoffHz, 1_000.000_000_1),
            (FilterParameter::Q, 0.750_000_000_1),
        ] {
            let knot =
                ParameterKnot::new(SessionFrame::new(10), value, CurveShape::HOLD).expect("point");
            let program = FilterProgram::new(field, vec![knot]).expect("program");
            assert_eq!(
                program.change_at(SessionFrame::new(10)).value().to_bits(),
                value.to_bits()
            );
        }
    }

    #[kithara::test]
    fn linear_cutoff_is_logarithmic_while_q_and_gain_keep_their_units() {
        for (field, left, right, midpoint) in [
            (FilterParameter::CutoffHz, 200.0, 800.0, 400.0),
            (FilterParameter::Q, 2.0, 6.0, 4.0),
            (FilterParameter::GainDb, -24.0, 6.0, -9.0),
        ] {
            let program = FilterProgram::new(
                field,
                vec![
                    ParameterKnot::new(SessionFrame::new(10), left, CurveShape::LINEAR)
                        .expect("left"),
                    ParameterKnot::new(SessionFrame::new(26), right, CurveShape::HOLD)
                        .expect("right"),
                ],
            )
            .expect("program");
            for (frame, value) in [(10, left), (18, midpoint), (26, right), (40, right)] {
                assert_eq!(program.change_at(SessionFrame::new(frame)).value(), value);
            }
        }
    }
}
