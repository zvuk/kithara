use kithara_dsp::curve::CurveShape;
use num_traits::ToPrimitive;

use crate::{EffectChange, EffectRefusal, EffectTarget, GainDb, eq::EqBandConfigChange};

/// One continuous field in an installed effect, in its declared units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EffectParameter {
    /// Canonical f32 dB gain, including exact kill at the range floor.
    EqGain { effect: usize, band: usize },
}

impl EffectParameter {
    /// Continuous field changed by a typed scalar command.
    #[must_use]
    pub const fn from_change(target: EffectTarget, change: EffectChange) -> Self {
        let (EffectTarget::EqBand { effect, band }, EffectChange::EqBand(_)) = (target, change);
        Self::EqGain { effect, band }
    }

    /// Configuration target containing this field.
    #[must_use]
    pub const fn target(self) -> EffectTarget {
        match self {
            Self::EqGain { effect, band } => EffectTarget::EqBand { effect, band },
        }
    }

    /// Interpolate in the concrete field's unit domain, copying exact endpoints.
    #[must_use]
    pub fn interpolate(self, left: f64, right: f64, progress: f64) -> f64 {
        if left == right || progress == 0.0 {
            left
        } else if progress == 1.0 {
            right
        } else {
            left + (right - left) * progress
        }
    }

    /// Convert a finite value without clamping an invalid program into range.
    /// # Errors
    /// Refuses a value outside the concrete parameter's domain.
    pub fn change(self, value: f64) -> Result<EffectChange, EffectRefusal> {
        match self {
            Self::EqGain { .. } => {
                if !value.is_finite()
                    || !(f64::from(f32::from(GainDb::MIN))..=f64::from(f32::from(GainDb::MAX)))
                        .contains(&value)
                {
                    return Err(EffectRefusal);
                }
                let gain = value.to_f32().ok_or(EffectRefusal)?;
                Ok(EffectChange::EqBand(EqBandConfigChange::GainDb(
                    GainDb::from(gain),
                )))
            }
        }
    }
}

/// A prepared segment on local integer frames, with the concrete field's mapping.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct EffectAutomation {
    parameter: EffectParameter,
    left: f64,
    right: f64,
    shape: CurveShape,
    position: u64,
    duration: u64,
}

impl EffectAutomation {
    /// Bind the current segment without transferring its owning program or clock.
    /// # Errors
    /// Refuses invalid values or a position outside the segment.
    pub fn new(
        parameter: EffectParameter,
        left: f64,
        right: f64,
        shape: CurveShape,
        position: u64,
        duration: u64,
    ) -> Result<Self, EffectRefusal> {
        parameter.change(left)?;
        parameter.change(right)?;
        if (duration == 0 && (left != right || position != 0))
            || (duration > 0 && position >= duration)
        {
            return Err(EffectRefusal);
        }
        Ok(Self {
            parameter,
            left,
            right,
            shape,
            position,
            duration,
        })
    }

    /// Addressed field in the installed chain.
    #[must_use]
    pub const fn parameter(self) -> EffectParameter {
        self.parameter
    }

    pub(crate) fn admits_frames(self, frames: usize) -> bool {
        if frames == 0 {
            return true;
        }
        u64::try_from(frames - 1)
            .ok()
            .and_then(|offset| self.position.checked_add(offset))
            .is_some_and(|last| self.duration == 0 || last < self.duration)
    }

    pub(crate) fn value_range(self) -> std::ops::RangeInclusive<f64> {
        self.left.min(self.right)..=self.left.max(self.right)
    }

    pub(crate) fn change(self, offset: usize) -> EffectChange {
        let value = if self.left == self.right {
            self.left
        } else {
            let position =
                self.position + u64::try_from(offset).expect("admitted local block fits u64");
            let progress = self.shape.progress(
                position.to_f64().expect("finite local frame")
                    / self.duration.to_f64().expect("positive finite duration"),
            );
            self.parameter.interpolate(self.left, self.right, progress)
        };
        self.parameter
            .change(value)
            .expect("prepared monotone segment remains in its parameter domain")
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const PARAMETER: EffectParameter = EffectParameter::EqGain { effect: 0, band: 0 };

    #[kithara::test]
    fn effect_segments_reject_invalid_values_and_cross_knot_blocks() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -24.01, 6.01] {
            assert!(
                EffectAutomation::new(PARAMETER, value, 0.0, CurveShape::LINEAR, 0, 80).is_err()
            );
            assert!(
                EffectAutomation::new(PARAMETER, 0.0, value, CurveShape::LINEAR, 0, 80).is_err()
            );
        }
        assert!(EffectAutomation::new(PARAMETER, 0.0, -12.0, CurveShape::LINEAR, 0, 0).is_err());
        assert!(EffectAutomation::new(PARAMETER, 0.0, -12.0, CurveShape::LINEAR, 80, 80).is_err());
        let span = EffectAutomation::new(PARAMETER, 0.0, -12.0, CurveShape::LINEAR, 3, 80).unwrap();
        assert!(span.admits_frames(77));
        assert!(!span.admits_frames(78));
        assert!(!span.admits_frames(usize::MAX));
        let held = EffectAutomation::new(PARAMETER, -12.0, -12.0, CurveShape::HOLD, 0, 0).unwrap();
        assert!(held.admits_frames(1024));
        assert_eq!(PARAMETER.interpolate(1.0, 1.0e-30, 0.0), 1.0);
        assert_eq!(PARAMETER.interpolate(1.0, 1.0e-30, 1.0), 1.0e-30);
    }

    #[kithara::test]
    fn effect_segments_follow_the_shared_polynomial_in_declared_db_units() {
        let cubic = EffectAutomation::new(
            PARAMETER,
            -12.0,
            0.0,
            CurveShape::cubic(0.0, 1.0).unwrap(),
            0,
            16,
        )
        .unwrap();
        for (at, expected) in [(0, -12.0), (4, -10.125), (8, -6.0), (12, -1.875)] {
            let EffectChange::EqBand(EqBandConfigChange::GainDb(gain)) = cubic.change(at);
            assert_eq!(f32::from(gain), expected);
        }
        let hold = EffectAutomation::new(PARAMETER, -12.0, 0.0, CurveShape::HOLD, 0, 16).unwrap();
        for at in 0..16 {
            let EffectChange::EqBand(EqBandConfigChange::GainDb(gain)) = hold.change(at);
            assert_eq!(f32::from(gain), -12.0);
        }
    }
}
