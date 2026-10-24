use std::num::{NonZeroU32, NonZeroU128};

use kithara_signal::ExponentialApproach;
use kithara_stretch::ElasticError;
use kithara_sync::SpeedCurve;
use num_traits::ToPrimitive;

use super::{Fraction, Phase, Trajectory};

pub(super) fn divisor(mut first: u128, mut second: u128) -> u128 {
    if second.is_power_of_two() {
        return 1u128 << first.trailing_zeros().min(second.trailing_zeros());
    }
    while second != 0 {
        (first, second) = (second, first % second);
    }
    first
}

fn check_precise_speed(value: f64) -> Result<(), ElasticError> {
    if !value.is_finite() || !(f64::from(kithara_sync::MIN_SPEED)..=4.0).contains(&value) {
        return Err(ElasticError::InvalidRate(value));
    }
    Ok(())
}

fn check_approach_steps(steps: &[(u64, ExponentialApproach)]) -> Result<(), ElasticError> {
    if steps.first().is_none_or(|(frame, _)| *frame != 0)
        || steps.windows(2).any(|pair| pair[0].0 >= pair[1].0)
    {
        return Err(ElasticError::EnginePreparation(
            "analytic segments must start at zero and be strictly ordered",
        ));
    }
    for (_, curve) in steps {
        check_precise_speed(curve.initial())?;
        check_precise_speed(curve.target())?;
    }
    Ok(())
}

fn check_speed_steps(steps: &[(u64, f32)]) -> Result<(), ElasticError> {
    if steps.is_empty() || steps.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(ElasticError::EnginePreparation(
            "speed steps must be nonempty and strictly ordered",
        ));
    }
    for (_, speed) in steps {
        Fraction::speed(*speed)?;
    }
    Ok(())
}

fn check_curve(curve: &SpeedCurve) -> Result<(), ElasticError> {
    match curve {
        SpeedCurve::Constant(speed) | SpeedCurve::Ramp { to: speed, .. } => {
            Fraction::speed(*speed)?;
        }
        SpeedCurve::Approach(curve) => {
            check_precise_speed(curve.initial())?;
            check_precise_speed(curve.target())?;
        }
        SpeedCurve::ApproachSteps(steps) => check_approach_steps(steps)?,
        SpeedCurve::Steps(steps) => check_speed_steps(steps)?,
    }
    Ok(())
}

impl Fraction {
    pub(in crate::render) fn on_lattice(self) -> Option<Self> {
        let scale = 1u128 << 32;
        let denominator = self.denominator.get();
        let frames = u64::try_from(self.numerator / denominator).ok()?;
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
            let mut remainder = self.numerator % denominator;
            let mut fraction = 0;
            for _ in 0..32 {
                fraction <<= 1;
                let complement = denominator - remainder;
                if remainder >= complement {
                    remainder -= complement;
                    fraction += 1;
                } else {
                    remainder += remainder;
                }
            }
            if remainder >= denominator - remainder {
                fraction += 1;
            }
            u128::from(frames) * scale + fraction
        };
        Self::new(numerator, scale)
    }

    pub(in crate::render) fn new(numerator: u128, denominator: u128) -> Option<Self> {
        let common = divisor(numerator, denominator);
        Some(Self {
            numerator: numerator.checked_div(common)?,
            denominator: NonZeroU128::new(denominator.checked_div(common)?)?,
        })
    }

    pub(super) fn speed(value: f32) -> Result<Self, ElasticError> {
        if !value.is_finite() || !(kithara_sync::MIN_SPEED..=4.0).contains(&value) {
            return Err(ElasticError::InvalidRate(f64::from(value)));
        }
        let bits = value.to_bits();
        let exponent =
            i32::try_from((bits >> 23) & 255).map_err(|_| ElasticError::SampleCountOverflow)? - 150;
        let mantissa = u128::from((bits & 0x7f_ffff) | 0x80_0000);
        let (numerator, denominator) = if exponent >= 0 {
            (mantissa.checked_shl(exponent.unsigned_abs()), Some(1))
        } else {
            (Some(mantissa), 1u128.checked_shl(exponent.unsigned_abs()))
        };
        numerator
            .zip(denominator)
            .and_then(|(numerator, denominator)| Self::new(numerator, denominator))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    fn precise_speed(value: f64) -> Result<Self, ElasticError> {
        check_precise_speed(value)?;
        let bits = value.to_bits();
        let exponent = i32::try_from((bits >> 52) & 2047)
            .map_err(|_| ElasticError::SampleCountOverflow)?
            - 1075;
        let mantissa = u128::from((bits & 0x000f_ffff_ffff_ffff) | 0x0010_0000_0000_0000);
        let (numerator, denominator) = if exponent >= 0 {
            (mantissa.checked_shl(exponent.unsigned_abs()), Some(1))
        } else {
            (Some(mantissa), 1u128.checked_shl(exponent.unsigned_abs()))
        };
        numerator
            .zip(denominator)
            .and_then(|(numerator, denominator)| Self::new(numerator, denominator))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(in crate::render) fn common(self, other: Self) -> Option<u128> {
        let first = self.denominator.get();
        first
            .checked_div(divisor(first, other.denominator.get()))?
            .checked_mul(other.denominator.get())
    }

    pub(in crate::render) fn at(self, denominator: u128) -> Option<u128> {
        self.numerator
            .checked_mul(denominator.checked_div(self.denominator.get())?)
    }

    fn value(self) -> Result<f32, ElasticError> {
        (self
            .numerator
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?
            / self
                .denominator
                .get()
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?)
        .to_f32()
        .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(in crate::render) fn correction(value: f64) -> Option<Self> {
        if !value.is_finite() || value <= 0.0 {
            return None;
        }
        let value = value.to_f32()?;
        if !value.is_finite() || value <= 0.0 {
            return None;
        }
        let bits = value.to_bits();
        let exponent = i32::try_from((bits >> 23) & 255).ok()? - 150;
        let mantissa = u128::from((bits & 0x7f_ffff) | 0x80_0000);
        let (numerator, denominator) = if exponent >= 0 {
            (mantissa.checked_shl(exponent.unsigned_abs())?, 1)
        } else {
            (mantissa, 1u128.checked_shl(exponent.unsigned_abs())?)
        };
        Self::new(numerator, denominator)
    }

    pub(in crate::render) fn add(self, other: Self) -> Option<Self> {
        let common = self.common(other)?;
        Self::new(self.at(common)?.checked_add(other.at(common)?)?, common)
    }

    pub(in crate::render) fn sub(self, other: Self) -> Option<Self> {
        let common = self.common(other)?;
        Self::new(self.at(common)?.checked_sub(other.at(common)?)?, common)
    }

    pub(in crate::render) fn mul(self, other: Self) -> Option<Self> {
        let first = divisor(self.numerator, other.denominator.get());
        let second = divisor(other.numerator, self.denominator.get());
        Self::new(
            (self.numerator / first).checked_mul(other.numerator / second)?,
            (self.denominator.get() / second).checked_mul(other.denominator.get() / first)?,
        )
    }

    pub(in crate::render) fn div(self, other: Self) -> Option<Self> {
        self.mul(Self {
            numerator: other.denominator.get(),
            denominator: NonZeroU128::new(other.numerator)?,
        })
    }

    pub(in crate::render) fn as_f64(self) -> Option<f64> {
        Some(self.numerator.to_f64()? / self.denominator.get().to_f64()?)
    }
}

impl Trajectory {
    /// Starts a speed law without an observed source phase.
    #[must_use]
    pub fn new(speed: f32) -> Self {
        Self {
            curve: SpeedCurve::Constant(speed),
            origin_speed: Fraction {
                numerator: 1,
                denominator: NonZeroU128::MIN,
            },
            elapsed: 0,
            position: None,
            curve_origin: None,
        }
    }

    pub(in crate::render) fn requires_quanta(&self) -> bool {
        !matches!(&self.curve, SpeedCurve::Constant(_))
    }

    pub(super) fn approach_at(&self, frame: u64) -> Option<(ExponentialApproach, u64)> {
        match &self.curve {
            SpeedCurve::Approach(curve) => Some((*curve, 0)),
            SpeedCurve::ApproachSteps(steps) => steps
                .get(
                    steps
                        .partition_point(|(at, _)| *at <= frame)
                        .checked_sub(1)?,
                )
                .map(|(at, curve)| (*curve, *at)),
            _ => None,
        }
    }

    pub(in crate::render) fn passthrough_speed(&self, speed: f32) -> bool {
        match self.approach_at(self.elapsed) {
            Some((curve, _)) => curve.initial() == 1.0 && curve.target() == 1.0,
            None => (speed - 1.0).abs() <= f32::EPSILON,
        }
    }

    pub(in crate::render) fn unity_interval(&self) -> bool {
        !matches!(&self.curve, SpeedCurve::Ramp { frames, .. } if self.elapsed < frames.get())
            && self.passthrough_speed(1.0)
            && self.speed().is_ok_and(|speed| speed == 1.0)
    }

    pub(in crate::render) fn constant_unity(&self) -> bool {
        self.unity_interval()
            && !match &self.curve {
                SpeedCurve::Steps(steps) => {
                    steps.last().is_some_and(|(frame, _)| *frame > self.elapsed)
                }
                SpeedCurve::ApproachSteps(steps) => {
                    steps.last().is_some_and(|(frame, _)| *frame > self.elapsed)
                }
                _ => false,
            }
    }

    pub(super) fn speed_at(&self) -> Result<Fraction, ElasticError> {
        match &self.curve {
            SpeedCurve::Constant(speed) => Fraction::speed(*speed),
            SpeedCurve::Approach(_) | SpeedCurve::ApproachSteps(_) => {
                let (curve, start) =
                    self.approach_at(self.elapsed)
                        .ok_or(ElasticError::EnginePreparation(
                            "analytic speed has no current segment",
                        ))?;
                (self.elapsed - start)
                    .to_f64()
                    .and_then(|elapsed| curve.rate_at(elapsed))
                    .ok_or(ElasticError::SampleCountOverflow)
                    .and_then(Fraction::precise_speed)
            }
            SpeedCurve::Steps(steps) => steps
                [..steps.partition_point(|(frame, _)| *frame <= self.elapsed)]
                .last()
                .map_or(Ok(self.origin_speed), |(_, speed)| Fraction::speed(*speed)),
            SpeedCurve::Ramp { to, frames } => {
                let target = Fraction::speed(*to)?;
                if self.elapsed >= frames.get() {
                    return Ok(target);
                }
                let elapsed = self.elapsed.min(frames.get());
                let denominator = self
                    .origin_speed
                    .common(target)
                    .and_then(|common| common.checked_mul(u128::from(frames.get())))
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let numerator = self
                    .origin_speed
                    .at(denominator)
                    .and_then(|origin| origin.checked_mul(u128::from(frames.get() - elapsed)))
                    .and_then(|origin| {
                        target
                            .at(denominator)
                            .and_then(|target| target.checked_mul(u128::from(elapsed)))
                            .and_then(|target| origin.checked_add(target))
                    })
                    .and_then(|total| total.checked_div(u128::from(frames.get())))
                    .ok_or(ElasticError::SampleCountOverflow)?;
                Fraction::new(numerator, denominator).ok_or(ElasticError::SampleCountOverflow)
            }
        }
    }

    /// Replaces the speed law with the renderer's inherited speed and Q32 phase policy.
    ///
    /// # Errors
    /// Rejects an invalid law or an unrepresentable source coordinate.
    pub fn replace(&mut self, curve: SpeedCurve) -> Result<f32, ElasticError> {
        check_curve(&curve)?;
        let origin_speed = match &self.curve {
            SpeedCurve::Approach(_) | SpeedCurve::ApproachSteps(_) => self.speed_at()?,
            _ => Fraction::speed(self.speed_at()?.value()?)?,
        };
        let position = match self.position {
            Some(position) => Some(
                position
                    .on_lattice()
                    .ok_or(ElasticError::SampleCountOverflow)?,
            ),
            None => None,
        };
        let previous = std::mem::replace(&mut self.curve, curve);
        let previous_origin = self.origin_speed;
        let previous_elapsed = self.elapsed;
        let previous_position = self.position;
        let previous_curve_origin = self.curve_origin;
        self.origin_speed = origin_speed;
        self.position = position;
        self.curve_origin = position
            .and_then(Phase::ratio)
            .map(|position| (position, 0));
        self.elapsed = 0;
        let result = self.speed_at().and_then(Fraction::value).and_then(|speed| {
            let frames = match &self.curve {
                SpeedCurve::Ramp { frames, .. } => frames.get(),
                _ => 1,
            };
            self.source_span(0, NonZeroU32::MIN, frames)?;
            Ok(speed)
        });
        if result.is_err() {
            self.curve = previous;
            self.origin_speed = previous_origin;
            self.elapsed = previous_elapsed;
            self.position = previous_position;
            self.curve_origin = previous_curve_origin;
        }
        result
    }

    pub(in crate::render) fn output_limit<T: Ord + TryFrom<u64>>(&self, budget: T) -> T {
        let next = match &self.curve {
            SpeedCurve::Steps(steps) => steps
                .get(steps.partition_point(|(frame, _)| *frame <= self.elapsed))
                .map(|(frame, _)| *frame),
            SpeedCurve::ApproachSteps(steps) => steps
                .get(steps.partition_point(|(frame, _)| *frame <= self.elapsed))
                .map(|(frame, _)| *frame),
            SpeedCurve::Ramp { frames, .. } if self.elapsed < frames.get() => Some(frames.get()),
            _ => None,
        };
        match next.and_then(|next| T::try_from(next - self.elapsed).ok()) {
            Some(next) => budget.min(next),
            None => budget,
        }
    }

    pub(in crate::render) fn speed(&self) -> Result<f32, ElasticError> {
        self.speed_at()?.value()
    }

    pub(in crate::render) fn speed_bounds(&self) -> Result<(f32, f32), ElasticError> {
        let origin = self.origin_speed.value()?;
        Ok(match &self.curve {
            SpeedCurve::Constant(speed) => (*speed, *speed),
            SpeedCurve::Approach(curve) => {
                let initial = curve
                    .initial()
                    .to_f32()
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let target = curve
                    .target()
                    .to_f32()
                    .ok_or(ElasticError::SampleCountOverflow)?;
                (initial.min(target), initial.max(target))
            }
            SpeedCurve::ApproachSteps(steps) => {
                let mut minimum = origin;
                let mut maximum = origin;
                for (_, curve) in steps.iter() {
                    let initial = curve
                        .initial()
                        .to_f32()
                        .ok_or(ElasticError::SampleCountOverflow)?;
                    let target = curve
                        .target()
                        .to_f32()
                        .ok_or(ElasticError::SampleCountOverflow)?;
                    minimum = minimum.min(initial).min(target);
                    maximum = maximum.max(initial).max(target);
                }
                (minimum, maximum)
            }
            SpeedCurve::Ramp { to, .. } => (origin.min(*to), origin.max(*to)),
            SpeedCurve::Steps(steps) => steps
                .iter()
                .fold((origin, origin), |(minimum, maximum), (_, speed)| {
                    (minimum.min(*speed), maximum.max(*speed))
                }),
        })
    }

    pub(in crate::render) fn projection_stages(
        &self,
        correction: f64,
    ) -> Result<usize, ElasticError> {
        let (minimum, maximum) = self.speed_bounds()?;
        let mut pitch = correction / f64::from(minimum);
        let mut stages = 1;
        while pitch > 4.0 {
            pitch /= 4.0;
            stages += 1;
        }
        let mut pitch = correction / f64::from(maximum);
        let mut fast_stages = 1;
        while pitch < 0.25 {
            pitch *= 4.0;
            fast_stages += 1;
        }
        stages = stages.max(fast_stages);
        if stages > 3 {
            return Err(ElasticError::InvalidRate(f64::from(minimum) / correction));
        }
        Ok(stages)
    }
}
