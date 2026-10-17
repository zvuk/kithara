use std::num::NonZeroU128;

use super::{affine::Affine, approach::Approach, ratio::Ratio};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mapping {
    Rational(Rational),
    Approach(Approach),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Rational {
    pub(super) numerator: u128,
    pub(super) denominator: NonZeroU128,
    pub(super) step: u128,
    pub(super) step_change: i128,
}

impl Rational {
    fn step_at(self, frame: u64) -> Option<u128> {
        let change = self
            .step_change
            .unsigned_abs()
            .checked_mul(u128::from(frame))?;
        if self.step_change < 0 {
            self.step.checked_sub(change)
        } else {
            self.step.checked_add(change)
        }
    }

    fn numerator_at(self, frame: u64) -> Option<u128> {
        let linear = self.step.checked_mul(u128::from(frame))?;
        let pairs = u128::from(frame).checked_mul(u128::from(frame.saturating_sub(1)))? / 2;
        let change = pairs.checked_mul(self.step_change.unsigned_abs())?;
        let advance = if self.step_change < 0 {
            linear.checked_sub(change)?
        } else {
            linear.checked_add(change)?
        };
        self.numerator.checked_add(advance)
    }

    fn sliced(self, start: u64, frames: u64) -> Option<Self> {
        Some(Self {
            numerator: self.numerator_at(start)?,
            step: if frames == 0 && self.step_change != 0 {
                0
            } else {
                self.step_at(start)?
            },
            ..self
        })
    }

    fn followed_by(self, next: Self, frames: u64) -> Option<Self> {
        if self.step_at(frames)? != next.step
            || self.step_change != next.step_change
            || self.denominator != next.denominator
            || self.numerator_at(frames)? != next.numerator
        {
            return None;
        }
        Some(self)
    }

    fn transformed_step_change(self, magnitude: u128) -> Option<i128> {
        if self.step_change < 0 && magnitude != 0 {
            Some(-i128::try_from(magnitude - 1).ok()? - 1)
        } else {
            i128::try_from(magnitude).ok()
        }
    }

    fn transformed(self, affine: Affine) -> Option<Self> {
        let origin = affine.apply(Ratio::new(self.numerator, self.denominator)?)?;
        let step = affine.scale(Ratio::new(self.step, self.denominator)?)?;
        let change = affine.scale(Ratio::new(
            self.step_change.unsigned_abs(),
            self.denominator,
        )?)?;
        let denominator = origin.common(step)?;
        let denominator = Ratio {
            numerator: 0,
            denominator,
        }
        .common(change)?;
        let magnitude = change.at(denominator)?;
        let step_change = self.transformed_step_change(magnitude)?;
        Some(Self {
            numerator: origin.at(denominator)?,
            denominator,
            step: step.at(denominator)?,
            step_change,
        })
    }
}

impl Mapping {
    pub(super) fn ratio_at(self, frame: u64) -> Option<Ratio> {
        match self {
            Self::Rational(mapping) => {
                Ratio::new(mapping.numerator_at(frame)?, mapping.denominator)
            }
            Self::Approach(mapping) => mapping.ratio_at(frame),
        }
    }

    pub(super) fn validate(self, frames: u64) -> Option<()> {
        if let Self::Rational(mapping) = self
            && frames > 0
        {
            mapping.step_at(frames - 1)?;
        }
        self.ratio_at(0)?.source_frame()?;
        self.ratio_at(frames)?.source_frame()?;
        Some(())
    }

    pub(super) fn sliced(self, start: u64, frames: u64) -> Option<Self> {
        match self {
            Self::Rational(mapping) => Some(Self::Rational(mapping.sliced(start, frames)?)),
            Self::Approach(mapping) => Some(Self::Approach(mapping.sliced(start)?)),
        }
    }

    pub(super) fn transformed(self, affine: Affine) -> Option<Self> {
        match self {
            Self::Rational(mapping) => Some(Self::Rational(mapping.transformed(affine)?)),
            Self::Approach(mapping) => Some(Self::Approach(mapping.transformed(affine)?)),
        }
    }

    pub(super) fn followed_by(self, next: Self, frames: u64) -> Option<Self> {
        match (self, next) {
            (Self::Rational(mapping), Self::Rational(next)) => {
                Some(Self::Rational(mapping.followed_by(next, frames)?))
            }
            (Self::Approach(mapping), Self::Approach(next)) => {
                Some(Self::Approach(mapping.followed_by(next, frames)?))
            }
            _ => None,
        }
    }
}
