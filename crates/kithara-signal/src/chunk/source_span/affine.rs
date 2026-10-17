use std::num::NonZeroU128;

use super::ratio::Ratio;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Affine {
    pub(super) scale: Ratio,
    pub(super) offset: Offset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Offset {
    Add(Ratio),
    Subtract(Ratio),
}

impl Offset {
    fn difference(first: Ratio, second: Ratio) -> Option<Self> {
        let denominator = first.common(second)?;
        let first = first.at(denominator)?;
        let second = second.at(denominator)?;
        if first >= second {
            Some(Self::Add(Ratio::new(first - second, denominator)?))
        } else {
            Some(Self::Subtract(Ratio::new(second - first, denominator)?))
        }
    }

    fn scaled(self, scale: Ratio) -> Option<Self> {
        match self {
            Self::Add(value) => Some(Self::Add(value.mul(scale)?)),
            Self::Subtract(value) => Some(Self::Subtract(value.mul(scale)?)),
        }
    }

    fn add(self, other: Self) -> Option<Self> {
        match (self, other) {
            (Self::Add(first), Self::Add(second)) => Some(Self::Add(first.add(second)?)),
            (Self::Subtract(first), Self::Subtract(second)) => {
                Some(Self::Subtract(first.add(second)?))
            }
            (Self::Add(first), Self::Subtract(second)) => Self::difference(first, second),
            (Self::Subtract(first), Self::Add(second)) => Self::difference(second, first),
        }
    }

    fn apply(self, value: Ratio) -> Option<Ratio> {
        match self {
            Self::Add(offset) => value.add(offset),
            Self::Subtract(offset) => value.sub(offset),
        }
    }
}

impl Affine {
    pub(super) fn new(pivot: Ratio, position: Ratio, scale: Ratio) -> Option<Self> {
        if scale.numerator == 0 {
            return None;
        }
        Some(Self {
            scale,
            offset: Offset::difference(position, pivot.mul(scale)?)?,
        })
    }

    pub(super) fn identity() -> Option<Self> {
        Some(Self {
            scale: Ratio::new(1, NonZeroU128::MIN)?,
            offset: Offset::Add(Ratio::new(0, NonZeroU128::MIN)?),
        })
    }

    pub(super) fn apply(self, value: Ratio) -> Option<Ratio> {
        self.offset.apply(value.mul(self.scale)?)
    }

    pub(super) fn scale(self, value: Ratio) -> Option<Ratio> {
        value.mul(self.scale)
    }

    pub(super) fn then(self, next: Self) -> Option<Self> {
        Some(Self {
            scale: self.scale.mul(next.scale)?,
            offset: self.offset.scaled(next.scale)?.add(next.offset)?,
        })
    }
}
