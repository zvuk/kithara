use std::num::NonZeroU128;

use num_traits::Float;

/// A nonnegative fraction with coprime numerator and denominator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Ratio {
    pub(super) numerator: u128,
    pub(super) denominator: NonZeroU128,
}

/// Greatest common divisor by Stein's binary algorithm: `u128` has no hardware
/// division, so shifts and subtractions replace Euclid's remainder loop.
pub(super) fn divisor(first: u128, second: u128) -> u128 {
    if first == 0 {
        return second;
    }
    if second == 0 {
        return first;
    }
    if second.is_power_of_two() {
        return 1u128 << first.trailing_zeros().min(second.trailing_zeros());
    }
    let shift = first.trailing_zeros().min(second.trailing_zeros());
    let mut odd = first >> first.trailing_zeros();
    let mut rest = second;
    loop {
        rest >>= rest.trailing_zeros();
        if odd > rest {
            (odd, rest) = (rest, odd);
        }
        rest -= odd;
        if rest == 0 {
            return odd << shift;
        }
    }
}

fn quotient(numerator: u128, denominator: NonZeroU128) -> u128 {
    if denominator.get().is_power_of_two() {
        numerator >> denominator.get().trailing_zeros()
    } else {
        numerator / denominator.get()
    }
}

impl Ratio {
    pub(super) fn new(numerator: u128, denominator: NonZeroU128) -> Option<Self> {
        let common = NonZeroU128::new(divisor(numerator, denominator.get()))?;
        Some(Self {
            numerator: quotient(numerator, common),
            denominator: NonZeroU128::new(quotient(denominator.get(), common))?,
        })
    }

    pub(super) fn at(self, denominator: NonZeroU128) -> Option<u128> {
        self.numerator
            .checked_mul(quotient(denominator.get(), self.denominator))
    }

    pub(super) fn common(first: NonZeroU128, second: NonZeroU128) -> Option<NonZeroU128> {
        let common = NonZeroU128::new(divisor(first.get(), second.get()))?;
        NonZeroU128::new(quotient(first.get(), common).checked_mul(second.get())?)
    }

    /// Combines coprime fractions over their least common denominator. Only the
    /// shared factor of the denominators can divide the combined numerator, so
    /// the result is reduced by a divisor of that factor instead of the whole
    /// common denominator.
    fn combine(self, other: Self, numerator: fn(u128, u128) -> Option<u128>) -> Option<Self> {
        let shared = NonZeroU128::new(divisor(self.denominator.get(), other.denominator.get()))?;
        let combined = numerator(
            self.numerator
                .checked_mul(quotient(other.denominator.get(), shared))?,
            other
                .numerator
                .checked_mul(quotient(self.denominator.get(), shared))?,
        )?;
        if combined == 0 {
            return Self::new(0, NonZeroU128::MIN);
        }
        let reduction = NonZeroU128::new(divisor(combined, shared.get()))?;
        Some(Self {
            numerator: quotient(combined, reduction),
            denominator: NonZeroU128::new(
                quotient(self.denominator.get(), shared)
                    .checked_mul(quotient(other.denominator.get(), reduction))?,
            )?,
        })
    }

    pub(super) fn add(self, other: Self) -> Option<Self> {
        if other.numerator == 0 {
            return Some(self);
        }
        if self.numerator == 0 {
            return Some(other);
        }
        self.combine(other, u128::checked_add)
    }

    pub(super) fn sub(self, other: Self) -> Option<Self> {
        if other.numerator == 0 {
            return Some(self);
        }
        self.combine(other, u128::checked_sub)
    }

    pub(super) fn mul(self, other: Self) -> Option<Self> {
        if self.numerator == self.denominator.get() {
            return Some(other);
        }
        if other.numerator == other.denominator.get() {
            return Some(self);
        }
        let first = NonZeroU128::new(divisor(self.numerator, other.denominator.get()))?;
        let second = NonZeroU128::new(divisor(other.numerator, self.denominator.get()))?;
        Some(Self {
            numerator: quotient(self.numerator, first)
                .checked_mul(quotient(other.numerator, second))?,
            denominator: NonZeroU128::new(
                quotient(self.denominator.get(), second)
                    .checked_mul(quotient(other.denominator.get(), first))?,
            )?,
        })
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
        u64::try_from(quotient(self.numerator, self.denominator)).ok()
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn source_quotients_match_full_width_integer_division() {
        let numerators = [0, 1, 3, u128::from(u64::MAX), 1 << 64, 1 << 127, u128::MAX];
        for bits in 0..u128::BITS {
            let denominator = NonZeroU128::new(1 << bits).expect("positive dyadic divisor");
            for numerator in numerators.into_iter().chain([
                denominator.get() - 1,
                denominator.get(),
                denominator.get() + 1,
            ]) {
                assert_eq!(
                    quotient(numerator, denominator),
                    numerator / denominator.get(),
                    "numerator={numerator}, denominator={denominator}",
                );
            }
        }
        for denominator in [3, 5, 44100, u128::from(u64::MAX), u128::MAX] {
            let denominator = NonZeroU128::new(denominator).expect("positive general divisor");
            for numerator in numerators {
                assert_eq!(
                    quotient(numerator, denominator),
                    numerator / denominator.get()
                );
            }
        }
    }

    #[kithara::test]
    fn binary_divisor_matches_euclid_across_full_width() {
        fn euclid(mut first: u128, mut second: u128) -> u128 {
            while second != 0 {
                (first, second) = (second, first % second);
            }
            first
        }
        let mut values = vec![
            0,
            1,
            2,
            3,
            44_100,
            48_000,
            96_000,
            u128::from(u32::MAX),
            u128::from(u64::MAX),
            1 << 64,
            (1 << 64) + 1,
            1 << 127,
            u128::MAX - 1,
            u128::MAX,
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C834_u128;
        for _ in 0..64 {
            state = state
                .wrapping_mul(0x2360_ED05_1FC6_5DA4_4385_DF64_9FCC_F645)
                .wrapping_add(0x5851_F42D_4C95_7F2D_1405_7B7E_F767_814F);
            values.push(state >> (state % 127));
            values.push((state >> 96) * 44_100 * 48_000);
        }
        for &first in &values {
            for &second in &values {
                assert_eq!(
                    divisor(first, second),
                    euclid(first, second),
                    "first={first}, second={second}",
                );
            }
        }
    }

    #[kithara::test]
    fn shared_factor_sums_match_full_common_denominator_reduction() {
        let reference = |first: Ratio, second: Ratio, add: bool| {
            let denominator = Ratio::common(first.denominator, second.denominator)?;
            let (first, second) = (first.at(denominator)?, second.at(denominator)?);
            let numerator = if add {
                first.checked_add(second)?
            } else {
                first.checked_sub(second)?
            };
            Ratio::new(numerator, denominator)
        };
        let mut components = vec![
            1,
            2,
            3,
            1 << 32,
            3 << 31,
            44_100,
            48_000,
            44_100 << 32,
            u128::from(u64::MAX),
            u128::MAX >> 1,
            u128::MAX,
        ];
        let mut state = 0x2545_F491_4F6C_DD1D_9E37_79B9_7F4A_7C15_u128;
        for _ in 0..24 {
            state = state
                .wrapping_mul(0x2360_ED05_1FC6_5DA4_4385_DF64_9FCC_F645)
                .wrapping_add(0x5851_F42D_4C95_7F2D_1405_7B7E_F767_814F);
            components.push((state >> (state % 125)).max(1));
        }
        let ratios: Vec<_> = components
            .iter()
            .flat_map(|&numerator| {
                components.iter().filter_map(move |&denominator| {
                    Ratio::new(numerator, NonZeroU128::new(denominator)?)
                })
            })
            .step_by(7)
            .collect();
        for &first in &ratios {
            for &second in &ratios {
                for add in [true, false] {
                    let combined = if add {
                        first.add(second)
                    } else {
                        first.sub(second)
                    };
                    if let Some(expected) = reference(first, second, add) {
                        assert_eq!(combined, Some(expected), "{first:?} {second:?} add={add}");
                    } else if let Some(value) = combined {
                        assert_eq!(divisor(value.numerator, value.denominator.get()), 1);
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn reduced_products_preserve_exact_cancellation_and_overflow() {
        let ratio = |numerator, denominator| {
            Ratio::new(
                numerator,
                NonZeroU128::new(denominator).expect("positive divisor"),
            )
            .expect("representable ratio")
        };
        for first in [0, 1, 2, 3, 7, 31] {
            for first_denominator in [1, 2, 3, 7, 31] {
                for second in [0, 1, 2, 3, 7, 31] {
                    for second_denominator in [1, 2, 3, 7, 31] {
                        let expected =
                            ratio(first * second, first_denominator * second_denominator);
                        assert_eq!(
                            ratio(first, first_denominator).mul(ratio(second, second_denominator)),
                            Some(expected),
                        );
                    }
                }
            }
        }
        let zero = ratio(0, 1);
        let full_width = ratio(u128::MAX, u128::MAX - 1);
        assert_eq!(zero.mul(full_width), Some(zero));
        assert_eq!(full_width.mul(zero), Some(zero));
        assert_eq!(
            full_width.mul(ratio(u128::MAX - 1, u128::MAX)),
            Some(ratio(1, 1)),
        );
        assert_eq!(
            ratio(u128::MAX, 2).mul(ratio(2, u128::MAX)),
            Some(ratio(1, 1)),
        );
        assert_eq!(ratio(u128::MAX, 1).mul(ratio(2, 1)), None);
        assert_eq!(ratio(1, u128::MAX).mul(ratio(1, 2)), None);
    }

    #[kithara::test]
    fn finite_float_ratios_keep_coprime_components() {
        let one = Ratio::new(1, NonZeroU128::MIN).expect("unit ratio");
        for value in [
            0.0,
            -0.0,
            0.5,
            1.5,
            0.1,
            2.0_f64.powi(-127),
            2.0_f64.powi(127),
        ] {
            let ratio = Ratio::try_from(value).expect("representable float ratio");
            assert_eq!(divisor(ratio.numerator, ratio.denominator.get()), 1);
            assert_eq!(ratio.mul(one), Some(ratio));
            assert_eq!(one.mul(ratio), Some(ratio));
        }
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            f64::MIN_POSITIVE,
            f64::MAX,
        ] {
            assert!(Ratio::try_from(value).is_err());
        }
    }

    #[kithara::test]
    fn rational_identities_preserve_full_width_values_and_failure_boundaries() {
        let ratio = |numerator, denominator| {
            Ratio::new(
                numerator,
                NonZeroU128::new(denominator).expect("positive divisor"),
            )
            .expect("representable ratio")
        };
        let zero = ratio(0, 1);
        let unit = ratio(1, 1);
        for value in [
            zero,
            unit,
            ratio(u128::MAX, u128::MAX - 1),
            ratio(u128::MAX, 1),
            ratio(1, u128::MAX),
            Ratio::try_from(0.1).expect("finite dyadic ratio"),
        ] {
            assert_eq!(zero.add(value), Some(value));
            assert_eq!(value.add(zero), Some(value));
            assert_eq!(value.sub(zero), Some(value));
            assert_eq!(unit.mul(value), Some(value));
            assert_eq!(value.mul(unit), Some(value));
        }
        assert_eq!(zero.sub(unit), None);
        assert_eq!(ratio(u128::MAX, 1).add(unit), None);
        let first = ratio(1, u128::MAX);
        let second = ratio(1, u128::MAX - 1);
        assert_eq!(first.add(second), None);
        assert_eq!(first.sub(second), None);
        assert_eq!(second.sub(first), None);
    }
}
