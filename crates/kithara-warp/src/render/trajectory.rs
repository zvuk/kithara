use std::num::{NonZeroU32, NonZeroU128};

use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;

use crate::{SpeedCurve, consts};

#[derive(Clone, Copy)]
pub(super) struct Fraction {
    pub(super) numerator: u128,
    pub(super) denominator: NonZeroU128,
}

fn divisor(mut first: u128, mut second: u128) -> u128 {
    while second != 0 {
        (first, second) = (second, first % second);
    }
    first
}

impl Fraction {
    pub(super) fn on_lattice(self) -> Option<Self> {
        let scale = 1u128 << 32;
        let denominator = self.denominator.get();
        let frames = u64::try_from(self.numerator / denominator).ok()?;
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
        Self::new(u128::from(frames) * scale + fraction, scale)
    }

    pub(super) fn new(numerator: u128, denominator: u128) -> Option<Self> {
        let common = divisor(numerator, denominator);
        Some(Self {
            numerator: numerator.checked_div(common)?,
            denominator: NonZeroU128::new(denominator.checked_div(common)?)?,
        })
    }

    fn speed(value: f32) -> Result<Self, ElasticError> {
        if !value.is_finite() || !(consts::MIN_SPEED..=4.0).contains(&value) {
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

    pub(super) fn common(self, other: Self) -> Option<u128> {
        let first = self.denominator.get();
        first
            .checked_div(divisor(first, other.denominator.get()))?
            .checked_mul(other.denominator.get())
    }

    pub(super) fn at(self, denominator: u128) -> Option<u128> {
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

    pub(super) fn correction(value: f64) -> Option<Self> {
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

    pub(super) fn add(self, other: Self) -> Option<Self> {
        let common = self.common(other)?;
        Self::new(self.at(common)?.checked_add(other.at(common)?)?, common)
    }

    pub(super) fn sub(self, other: Self) -> Option<Self> {
        let common = self.common(other)?;
        Self::new(self.at(common)?.checked_sub(other.at(common)?)?, common)
    }

    pub(super) fn mul(self, other: Self) -> Option<Self> {
        let first = divisor(self.numerator, other.denominator.get());
        let second = divisor(other.numerator, self.denominator.get());
        Self::new(
            (self.numerator / first).checked_mul(other.numerator / second)?,
            (self.denominator.get() / second).checked_mul(other.denominator.get() / first)?,
        )
    }

    pub(super) fn div(self, other: Self) -> Option<Self> {
        self.mul(Self {
            numerator: other.denominator.get(),
            denominator: NonZeroU128::new(other.numerator)?,
        })
    }

    pub(super) fn as_f64(self) -> Option<f64> {
        Some(self.numerator.to_f64()? / self.denominator.get().to_f64()?)
    }
}

#[derive(Clone)]
pub(super) struct Trajectory {
    curve: SpeedCurve,
    origin_speed: Fraction,
    elapsed: u64,
    position: Option<Phase>,
}

/// Replacement origins use whole source frames and Q32 fractions, rounded to
/// nearest with exact half units toward the next frame. A running curve keeps
/// its exact integral until the next replacement.
#[derive(Clone, Copy)]
enum Phase {
    Origin { frames: u64, fraction: u32 },
    Rendered(SourceSpan),
}

impl Phase {
    fn ratio(self) -> Option<Fraction> {
        match self {
            Self::Origin { frames, fraction } => Fraction::new(
                (u128::from(frames) << 32) + u128::from(fraction),
                1u128 << 32,
            ),
            Self::Rendered(span) => {
                let (numerator, denominator) = span.source_ratio_at(span.output_frames())?;
                Some(Fraction {
                    numerator,
                    denominator,
                })
            }
        }
    }

    fn on_lattice(self) -> Option<Self> {
        let position = self.ratio()?.on_lattice()?;
        let numerator = position.at(1u128 << 32)?;
        Some(Self::Origin {
            frames: u64::try_from(numerator >> 32).ok()?,
            fraction: u32::try_from(numerator & u128::from(u32::MAX)).ok()?,
        })
    }
}

impl Trajectory {
    pub(super) fn new(speed: f32) -> Self {
        Self {
            curve: SpeedCurve::Constant(speed),
            origin_speed: Fraction {
                numerator: 1,
                denominator: NonZeroU128::MIN,
            },
            elapsed: 0,
            position: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.position = None;
    }

    pub(super) fn snap_position(&mut self) -> Result<(), ElasticError> {
        if let Some(position) = self.position {
            self.position = Some(
                position
                    .on_lattice()
                    .ok_or(ElasticError::SampleCountOverflow)?,
            );
        }
        Ok(())
    }

    pub(super) fn snap_to_frame(&mut self) -> Result<(), ElasticError> {
        if let Some(position) = self.position {
            let position = position.ratio().ok_or(ElasticError::SampleCountOverflow)?;
            let denominator = position.denominator.get();
            let remainder = position.numerator % denominator;
            let frames =
                position.numerator / denominator + u128::from(remainder >= denominator - remainder);
            self.position = Some(Phase::Origin {
                frames: u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
                fraction: 0,
            });
        }
        Ok(())
    }

    pub(super) fn requires_quanta(&self) -> bool {
        !matches!(self.curve, SpeedCurve::Constant(_))
    }

    pub(super) fn unity_interval(&self) -> bool {
        !matches!(&self.curve, SpeedCurve::Ramp { frames, .. } if self.elapsed < frames.get())
            && self.speed().is_ok_and(|speed| speed == 1.0)
    }

    pub(super) fn constant_unity(&self) -> bool {
        self.unity_interval()
            && !matches!(
                &self.curve,
                SpeedCurve::Steps(steps)
                    if steps.last().is_some_and(|(frame, _)| *frame > self.elapsed)
            )
    }

    fn speed_at(&self) -> Result<Fraction, ElasticError> {
        match &self.curve {
            SpeedCurve::Constant(speed) => Fraction::speed(*speed),
            SpeedCurve::Steps(steps) => steps
                .iter()
                .rev()
                .find(|(frame, _)| *frame <= self.elapsed)
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

    pub(super) fn replace(&mut self, curve: SpeedCurve) -> Result<f32, ElasticError> {
        match &curve {
            SpeedCurve::Constant(speed) | SpeedCurve::Ramp { to: speed, .. } => {
                Fraction::speed(*speed)?;
            }
            SpeedCurve::Steps(steps) => {
                if steps.is_empty() || steps.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
                    return Err(ElasticError::EnginePreparation(
                        "speed steps must be nonempty and strictly ordered",
                    ));
                }
                for (_, speed) in steps.iter() {
                    Fraction::speed(*speed)?;
                }
            }
        }
        let origin_speed = Fraction::speed(self.speed_at()?.value()?)?;
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
        self.origin_speed = origin_speed;
        self.position = position;
        self.elapsed = 0;
        let result = self.speed_at().and_then(Fraction::value).and_then(|speed| {
            let frames = match &self.curve {
                SpeedCurve::Ramp { frames, .. } => frames.get(),
                _ => 1,
            };
            self.span(
                0,
                NonZeroU32::MIN,
                usize::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
            )?;
            Ok(speed)
        });
        if result.is_err() {
            self.curve = previous;
            self.origin_speed = previous_origin;
            self.elapsed = previous_elapsed;
            self.position = previous_position;
        }
        result
    }

    pub(super) fn output_limit(&self, budget: usize) -> usize {
        let next = match &self.curve {
            SpeedCurve::Steps(steps) => steps
                .iter()
                .find(|(frame, _)| *frame > self.elapsed)
                .map(|(frame, _)| *frame),
            SpeedCurve::Ramp { frames, .. } if self.elapsed < frames.get() => Some(frames.get()),
            _ => None,
        };
        next.map_or(budget, |next| {
            budget.min(usize::try_from(next - self.elapsed).unwrap_or(usize::MAX))
        })
    }

    pub(super) fn speed(&self) -> Result<f32, ElasticError> {
        self.speed_at()?.value()
    }

    pub(super) fn unity_span(
        &self,
        start: u64,
        sample_rate: NonZeroU32,
        frames: usize,
    ) -> Result<SourceSpan, ElasticError> {
        let position = self.source_position(start)?;
        SourceSpan::try_from((
            position.numerator,
            position.denominator.get(),
            position.denominator,
            sample_rate,
            u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
        ))
        .ok()
        .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(super) fn speed_bounds(&self) -> Result<(f32, f32), ElasticError> {
        let origin = self.origin_speed.value()?;
        Ok(match &self.curve {
            SpeedCurve::Constant(speed) => (*speed, *speed),
            SpeedCurve::Ramp { to, .. } => (origin.min(*to), origin.max(*to)),
            SpeedCurve::Steps(steps) => steps
                .iter()
                .fold((origin, origin), |(minimum, maximum), (_, speed)| {
                    (minimum.min(*speed), maximum.max(*speed))
                }),
        })
    }

    pub(super) fn projection_stages(&self, correction: f64) -> Result<usize, ElasticError> {
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

    pub(super) fn at_offset(
        &self,
        start: u64,
        rate: NonZeroU32,
        offset: u64,
    ) -> Result<(u128, NonZeroU128), ElasticError> {
        let mut cursor = self.clone();
        let mut remaining = offset;
        while remaining > 0 {
            let frames = cursor.output_limit(
                usize::try_from(remaining).map_err(|_| ElasticError::SampleCountOverflow)?,
            );
            let span = cursor.span(start, rate, frames)?;
            cursor.advance(span)?;
            remaining -= span.output_frames();
        }
        cursor
            .span(start, rate, 1)?
            .source_ratio_at(0)
            .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(super) fn span(
        &self,
        start: u64,
        sample_rate: NonZeroU32,
        frames: usize,
    ) -> Result<SourceSpan, ElasticError> {
        let position = self.source_position(start)?;
        let speed = self.speed_at()?;
        let common = position
            .common(speed)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let mut denominator = common;
        let mut step = speed.at(common).ok_or(ElasticError::SampleCountOverflow)?;
        let mut change = 0i128;
        if let SpeedCurve::Ramp {
            to,
            frames: duration,
        } = &self.curve
            && self.elapsed < duration.get()
        {
            let target = Fraction::speed(*to)?;
            let basis = self
                .origin_speed
                .common(target)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let delta = i128::try_from(target.at(basis).ok_or(ElasticError::SampleCountOverflow)?)
                .ok()
                .and_then(|target| {
                    i128::try_from(self.origin_speed.at(basis)?)
                        .ok()
                        .and_then(|origin| target.checked_sub(origin))
                })
                .ok_or(ElasticError::SampleCountOverflow)?;
            let ramp_denominator = basis
                .checked_mul(u128::from(duration.get()))
                .and_then(|value| value.checked_mul(2))
                .ok_or(ElasticError::SampleCountOverflow)?;
            denominator = common
                .checked_div(divisor(common, ramp_denominator))
                .and_then(|value| value.checked_mul(ramp_denominator))
                .ok_or(ElasticError::SampleCountOverflow)?;
            let scaled_delta = delta
                .checked_mul(
                    i128::try_from(denominator / ramp_denominator)
                        .map_err(|_| ElasticError::SampleCountOverflow)?,
                )
                .ok_or(ElasticError::SampleCountOverflow)?;
            step = u128::try_from(
                i128::try_from(
                    speed
                        .at(denominator)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                )
                .map_err(|_| ElasticError::SampleCountOverflow)?
                .checked_add(scaled_delta)
                .ok_or(ElasticError::SampleCountOverflow)?,
            )
            .map_err(|_| ElasticError::SampleCountOverflow)?;
            change = scaled_delta
                .checked_mul(2)
                .ok_or(ElasticError::SampleCountOverflow)?;
        }
        SourceSpan::try_from((
            position
                .at(denominator)
                .ok_or(ElasticError::SampleCountOverflow)?,
            step,
            change,
            NonZeroU128::new(denominator).ok_or(ElasticError::SampleCountOverflow)?,
            sample_rate,
            u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
        ))
        .ok()
        .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(super) fn advance(&mut self, span: SourceSpan) -> Result<(), ElasticError> {
        let elapsed = self
            .elapsed
            .checked_add(span.output_frames())
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.position = Some(Phase::Rendered(span));
        self.elapsed = elapsed;
        Ok(())
    }

    fn source_position(&self, start: u64) -> Result<Fraction, ElasticError> {
        self.position.map_or_else(
            || {
                Ok(Fraction {
                    numerator: u128::from(start),
                    denominator: NonZeroU128::MIN,
                })
            },
            |position| position.ratio().ok_or(ElasticError::SampleCountOverflow),
        )
    }
}
