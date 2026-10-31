use kithara_effects::{EffectAutomation, EffectChange, EffectParameter};
use kithara_signal::SessionFrame;

use super::{ParameterCurve, ParameterCurveError, ParameterKnot};

/// Immutable audio-time values for one typed field of an installed effect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EffectProgram {
    parameter: EffectParameter,
    curve: ParameterCurve,
}

impl EffectProgram {
    pub const MAX_KNOTS: usize = ParameterCurve::MAX_KNOTS;
    pub const MAX_ACTIVE: usize = 16;

    /// Prepare ordered knots in the parameter's declared unit domain.
    /// # Errors
    /// Refuses invalid frames, capacity or values before a program can be sent.
    pub fn new(
        parameter: EffectParameter,
        knots: Vec<ParameterKnot>,
    ) -> Result<Self, ParameterCurveError> {
        let curve = ParameterCurve::new(knots)?;
        let range = curve.value_range();
        if parameter.change(*range.start()).is_err() || parameter.change(*range.end()).is_err() {
            return Err(ParameterCurveError::Range);
        }
        Ok(Self { parameter, curve })
    }

    #[must_use]
    pub const fn parameter(&self) -> EffectParameter {
        self.parameter
    }

    delegate::delegate! {
        to self.curve {
            /// First addressed session frame.
            #[must_use]
            pub fn start(&self) -> SessionFrame;
            /// Final addressed session frame; the terminal value remains applied.
            #[must_use]
            pub fn end(&self) -> SessionFrame;
            pub(crate) fn next_offset(&self, elapsed: u64) -> Option<u64>;
            pub(crate) fn value_range(&self) -> std::ops::RangeInclusive<f64>;
        }
    }

    pub(crate) fn value(&self, elapsed: u64) -> f64 {
        let (left, right, progress) = self.curve.segment_at(elapsed);
        self.parameter.interpolate(left, right, progress)
    }

    pub(crate) fn automation(&self, elapsed: u64) -> EffectAutomation {
        let segment = self.curve.segment(elapsed);
        EffectAutomation::new(
            self.parameter,
            segment.left,
            segment.right,
            segment.shape,
            segment.position,
            segment.duration,
        )
        .expect("prepared program retains a valid local segment")
    }

    pub(crate) fn change(&self, elapsed: u64) -> EffectChange {
        self.parameter
            .change(self.value(elapsed))
            .expect("prepared monotone curve remains in its parameter domain")
    }
}
