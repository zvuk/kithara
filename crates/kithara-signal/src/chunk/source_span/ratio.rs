use std::num::NonZeroU128;

use num_traits::Float;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Ratio {
    pub(super) numerator: u128,
    pub(super) denominator: NonZeroU128,
}

pub(super) fn divisor(mut first: u128, mut second: u128) -> u128 {
    if second.is_power_of_two() {
        return 1u128 << first.trailing_zeros().min(second.trailing_zeros());
    }
    while second != 0 {
        (first, second) = (second, first % second);
    }
    first
}

impl Ratio {
    pub(super) fn new(numerator: u128, denominator: NonZeroU128) -> Option<Self> {
        let common = divisor(numerator, denominator.get());
        Some(Self {
            numerator: numerator / common,
            denominator: NonZeroU128::new(denominator.get() / common)?,
        })
    }

    pub(super) fn at(self, denominator: NonZeroU128) -> Option<u128> {
        self.numerator
            .checked_mul(denominator.get().checked_div(self.denominator.get())?)
    }

    pub(super) fn common(self, other: Self) -> Option<NonZeroU128> {
        NonZeroU128::new(
            self.denominator
                .get()
                .checked_div(divisor(self.denominator.get(), other.denominator.get()))?
                .checked_mul(other.denominator.get())?,
        )
    }

    pub(super) fn add(self, other: Self) -> Option<Self> {
        let denominator = self.common(other)?;
        Self::new(
            self.at(denominator)?.checked_add(other.at(denominator)?)?,
            denominator,
        )
    }

    pub(super) fn sub(self, other: Self) -> Option<Self> {
        let denominator = self.common(other)?;
        Self::new(
            self.at(denominator)?.checked_sub(other.at(denominator)?)?,
            denominator,
        )
    }

    pub(super) fn mul(self, other: Self) -> Option<Self> {
        let first = divisor(self.numerator, other.denominator.get());
        let second = divisor(other.numerator, self.denominator.get());
        Self::new(
            (self.numerator / first).checked_mul(other.numerator / second)?,
            NonZeroU128::new(
                (self.denominator.get() / second).checked_mul(other.denominator.get() / first)?,
            )?,
        )
    }

    pub(super) fn nearest_q32(self) -> Option<Self> {
        let denominator = self.denominator.get();
        let units = 1u128 << 32;
        let numerator = if denominator.is_power_of_two() {
            let bits = denominator.trailing_zeros();
            if bits <= 32 {
                self.numerator.checked_mul(1u128 << (32 - bits))?
            } else {
                let bits = bits - 32;
                let rounded = (self.numerator & ((1u128 << bits) - 1)) >= (1u128 << (bits - 1));
                (self.numerator >> bits).checked_add(u128::from(rounded))?
            }
        } else {
            let whole = (self.numerator / denominator).checked_mul(units)?;
            let mut remainder = self.numerator % denominator;
            let mut fractional = 0;
            for _ in 0..32 {
                fractional *= 2;
                let complement = denominator - remainder;
                if remainder >= complement {
                    remainder -= complement;
                    fractional += 1;
                } else {
                    remainder *= 2;
                }
            }
            whole
                .checked_add(fractional)?
                .checked_add(u128::from(remainder >= denominator - remainder))?
        };
        Self::new(numerator, NonZeroU128::new(units)?)
    }

    pub(super) fn source_frame(self) -> Option<u64> {
        u64::try_from(self.numerator / self.denominator.get()).ok()
    }
}

impl TryFrom<f64> for Ratio {
    type Error = ();

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if !value.is_finite() || value < 0.0 {
            return Err(());
        }
        if value == 0.0 {
            return Self::new(0, NonZeroU128::MIN).ok_or(());
        }
        let (mantissa, exponent, _) = value.integer_decode();
        let mut numerator = u128::from(mantissa);
        let mut exponent = i32::from(exponent);
        if exponent < 0 {
            let reduction = numerator.trailing_zeros().min(exponent.unsigned_abs());
            numerator >>= reduction;
            exponent += i32::try_from(reduction).map_err(|_| ())?;
        }
        let power = 1u128.checked_shl(exponent.unsigned_abs()).ok_or(())?;
        let denominator = if exponent >= 0 {
            numerator = numerator.checked_mul(power).ok_or(())?;
            NonZeroU128::MIN
        } else {
            NonZeroU128::new(power).ok_or(())?
        };
        Ok(Self {
            numerator,
            denominator,
        })
    }
}
