use kithara_command::{ScopeId, Seq};
use kithara_effects::GainDb;
use kithara_signal::{SessionEpoch, SessionFrame};

use super::{ParameterCurve, ParameterCurveError, ParameterKnot};

/// Prepared gain of one deck EQ band, in canonical dB units.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
#[non_exhaustive]
pub struct EqGainProgram {
    #[field(skip)]
    band: usize,
    curve: ParameterCurve,
}

impl EqGainProgram {
    pub const MAX_KNOTS: usize = ParameterCurve::MAX_KNOTS;
    pub const MAX_ACTIVE: usize = 16;

    /// Prepare finite, ordered knots within the canonical gain range.
    /// # Errors
    /// Refuses malformed knots and gain values outside the target's range.
    pub fn new(band: usize, knots: Vec<ParameterKnot>) -> Result<Self, ParameterCurveError> {
        let curve = ParameterCurve::new(knots)?;
        let range = curve.value_range();
        if *range.start() < f64::from(f32::from(GainDb::MIN))
            || *range.end() > f64::from(f32::from(GainDb::MAX))
        {
            return Err(ParameterCurveError::Range);
        }
        Ok(Self { band, curve })
    }

    /// The band addressed by this program.
    #[must_use]
    pub fn band(&self) -> usize {
        self.band
    }

    delegate::delegate! {
        to self.curve {
            /// First addressed session frame.
            #[must_use]
            pub fn start(&self) -> SessionFrame;
            /// Final addressed frame, whose gain is held afterward.
            #[must_use]
            pub fn end(&self) -> SessionFrame;
        }
    }
}

/// The EQ layout and session axis on which a program was prepared.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
pub struct EqBinding {
    #[field(get, copy)]
    scope: ScopeId,
    #[field(get, copy)]
    layout: Option<Seq>,
    #[field(get, copy)]
    epoch: SessionEpoch,
}

impl EqBinding {
    pub(crate) fn new(scope: ScopeId, layout: Option<Seq>, epoch: SessionEpoch) -> Self {
        Self {
            scope,
            layout,
            epoch,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_dsp::curve::CurveShape;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn eq_programs_reject_out_of_range_gain_without_clamping_endpoints() {
        for value in [-24.01, 6.01] {
            let knot = ParameterKnot::new(SessionFrame::new(100), value, CurveShape::HOLD)
                .expect("finite knot");
            assert!(matches!(
                EqGainProgram::new(0, vec![knot]),
                Err(ParameterCurveError::Range)
            ));
        }
        for value in [-24.0, 6.0] {
            let knot = ParameterKnot::new(SessionFrame::new(100), value, CurveShape::HOLD)
                .expect("finite endpoint");
            let program = EqGainProgram::new(0, vec![knot]).expect("exact admitted endpoint");
            assert_eq!(program.curve().segment_at(0), (value, value, 0.0));
        }
    }
}
