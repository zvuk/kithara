use std::num::NonZeroU128;

use num_traits::ToPrimitive;

use super::{affine::Affine, ratio::Ratio};
use crate::ExponentialApproach;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Approach {
    pub(super) curve: ExponentialApproach,
    pub(super) elapsed: u64,
    pub(super) origin: Ratio,
    pub(super) affine: Affine,
}

impl Approach {
    fn advance_at(self, elapsed: u64) -> Option<Ratio> {
        let linear = Ratio::try_from(self.curve.target())
            .ok()?
            .mul(Ratio::new(u128::from(elapsed), NonZeroU128::MIN)?)?;
        let transient = self.curve.transient_advance(elapsed.to_f64()?)?;
        let correction = Ratio::try_from(transient.abs()).ok()?;
        if transient < 0.0 {
            linear.sub(correction)?.nearest_q32()
        } else {
            linear.add(correction)?.nearest_q32()
        }
    }

    pub(super) fn ratio_at(self, frame: u64) -> Option<Ratio> {
        let elapsed = self.elapsed.checked_add(frame)?;
        self.affine
            .apply(self.origin.add(self.advance_at(elapsed)?)?)
    }

    pub(super) fn sliced(self, start: u64) -> Option<Self> {
        Some(Self {
            elapsed: self.elapsed.checked_add(start)?,
            ..self
        })
    }

    pub(super) fn transformed(self, affine: Affine) -> Option<Self> {
        Some(Self {
            affine: self.affine.then(affine)?,
            ..self
        })
    }

    pub(super) fn followed_by(self, next: Self, frames: u64) -> Option<Self> {
        if self.curve != next.curve
            || self.origin != next.origin
            || self.affine != next.affine
            || self.elapsed.checked_add(frames)? != next.elapsed
        {
            return None;
        }
        Some(self)
    }
}
