use std::num::{NonZeroU32, NonZeroU128};

use kithara_signal::{ExponentialApproach, SourceSpan};
use kithara_stretch::ElasticError;
use kithara_sync::SpeedCurve;
use num_traits::ToPrimitive;

use super::{Fraction, Phase, Trajectory, core::divisor};

impl Phase {
    pub(super) fn ratio(self) -> Option<Fraction> {
        match self {
            Self::Exact(position) => Some(position),
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

    pub(super) fn on_lattice(self) -> Option<Self> {
        let position = self.ratio()?.on_lattice()?;
        let numerator = position.at(1u128 << 32)?;
        Some(Self::Origin {
            frames: u64::try_from(numerator >> 32).ok()?,
            fraction: u32::try_from(numerator & u128::from(u32::MAX)).ok()?,
        })
    }
}

impl Trajectory {
    /// Discards source phase after a discontinuity while retaining the running law.
    pub fn reset(&mut self) {
        self.position = None;
        self.curve_origin = None;
    }

    pub(in crate::render) fn snap_position(&mut self) -> Result<(), ElasticError> {
        if let Some(position) = self.position {
            self.position = Some(
                position
                    .on_lattice()
                    .ok_or(ElasticError::SampleCountOverflow)?,
            );
        }
        if self.approach_at(self.elapsed).is_some() {
            self.curve_origin = self
                .position
                .and_then(Phase::ratio)
                .map(|position| (position, self.elapsed));
        }
        Ok(())
    }

    pub(in crate::render) fn snap_to_frame(&mut self) -> Result<(), ElasticError> {
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
        if self.approach_at(self.elapsed).is_some() {
            self.curve_origin = self
                .position
                .and_then(Phase::ratio)
                .map(|position| (position, self.elapsed));
        }
        Ok(())
    }

    pub(in crate::render) fn unity_span(
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

    /// Projects an output boundary through the renderer's exact source law.
    ///
    /// # Errors
    /// Returns an error for unrepresentable source or output coordinates.
    pub(in crate::render) fn at_offset(
        &self,
        start: u64,
        rate: NonZeroU32,
        offset: u64,
    ) -> Result<(u128, NonZeroU128), ElasticError> {
        if matches!(&self.curve, SpeedCurve::Approach(_)) {
            let elapsed = self
                .elapsed
                .checked_add(offset)
                .ok_or(ElasticError::SampleCountOverflow)?;
            return self
                .approach_span(start, rate, elapsed, 1)?
                .source_ratio_at(0)
                .ok_or(ElasticError::SampleCountOverflow);
        }
        let mut cursor = self.clone();
        if offset > 0 {
            cursor.advance_by(start, rate, offset)?;
        }
        if cursor.approach_at(cursor.elapsed).is_some() {
            return cursor
                .approach_span(start, rate, cursor.elapsed, 1)?
                .source_ratio_at(0)
                .ok_or(ElasticError::SampleCountOverflow);
        }
        let position = cursor.source_position(start)?;
        Ok((position.numerator, position.denominator))
    }

    pub(in crate::render) fn mapping_origin(
        &self,
        start: u64,
    ) -> Result<(u128, NonZeroU128), ElasticError> {
        let position = if self.approach_at(self.elapsed).is_some() {
            self.curve_origin
                .map_or_else(|| self.source_position(start), |(position, _)| Ok(position))?
        } else {
            self.source_position(start)?
        };
        Ok((position.numerator, position.denominator))
    }

    /// Advances the accepted law across its own segment boundaries.
    ///
    /// # Errors
    /// Returns an error for unrepresentable source or output coordinates.
    #[kithara_test_macros::hang_watchdog]
    pub fn advance_by(
        &mut self,
        start: u64,
        rate: NonZeroU32,
        offset: u64,
    ) -> Result<(), ElasticError> {
        let mut remaining = offset;
        while remaining > 0 {
            hang_tick!();
            let frames = self.output_limit(remaining);
            let span = self.source_span(start, rate, frames)?;
            self.advance(span)?;
            remaining -= span.output_frames();
            hang_reset!();
        }
        Ok(())
    }

    pub(in crate::render) fn span(
        &self,
        start: u64,
        sample_rate: NonZeroU32,
        frames: usize,
    ) -> Result<SourceSpan, ElasticError> {
        self.source_span(
            start,
            sample_rate,
            u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
        )
    }

    fn ramp_coefficients(
        &self,
        speed: Fraction,
        common: u128,
        to: f32,
        duration: u64,
    ) -> Result<(u128, u128, i128), ElasticError> {
        let target = Fraction::speed(to)?;
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
            .checked_mul(u128::from(duration))
            .and_then(|value| value.checked_mul(2))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let denominator = common
            .checked_div(divisor(common, ramp_denominator))
            .and_then(|value| value.checked_mul(ramp_denominator))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scaled_delta = delta
            .checked_mul(
                i128::try_from(denominator / ramp_denominator)
                    .map_err(|_| ElasticError::SampleCountOverflow)?,
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        let step = u128::try_from(
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
        let change = scaled_delta
            .checked_mul(2)
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((denominator, step, change))
    }

    pub(super) fn source_span(
        &self,
        start: u64,
        sample_rate: NonZeroU32,
        frames: u64,
    ) -> Result<SourceSpan, ElasticError> {
        if self.approach_at(self.elapsed).is_some() {
            return self.approach_span(start, sample_rate, self.elapsed, self.output_limit(frames));
        }
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
            (denominator, step, change) =
                self.ramp_coefficients(speed, common, *to, duration.get())?;
        }
        SourceSpan::try_from((
            position
                .at(denominator)
                .ok_or(ElasticError::SampleCountOverflow)?,
            step,
            change,
            NonZeroU128::new(denominator).ok_or(ElasticError::SampleCountOverflow)?,
            sample_rate,
            frames,
        ))
        .ok()
        .ok_or(ElasticError::SampleCountOverflow)
    }

    fn approach_span(
        &self,
        start: u64,
        sample_rate: NonZeroU32,
        elapsed: u64,
        frames: u64,
    ) -> Result<SourceSpan, ElasticError> {
        let (curve, segment) = self
            .approach_at(elapsed)
            .ok_or(ElasticError::EnginePreparation(
                "analytic mapping has no current segment",
            ))?;
        let (origin, basis) = self.curve_origin.map_or_else(
            || {
                self.source_position(start)
                    .map(|position| (position, self.elapsed))
            },
            Ok,
        )?;
        let shifted = basis
            .checked_sub(segment)
            .and_then(|elapsed| elapsed.to_f64())
            .and_then(|elapsed| curve.rate_at(elapsed))
            .and_then(|initial| {
                ExponentialApproach::new(initial, curve.target(), curve.time_constant())
            })
            .ok_or(ElasticError::SampleCountOverflow)?;
        SourceSpan::approaching(
            origin.numerator,
            origin.denominator,
            shifted,
            elapsed
                .checked_sub(basis)
                .ok_or(ElasticError::SampleCountOverflow)?,
            sample_rate,
            frames,
        )
        .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(in crate::render) fn advance(&mut self, span: SourceSpan) -> Result<(), ElasticError> {
        let elapsed = self
            .elapsed
            .checked_add(span.output_frames())
            .ok_or(ElasticError::SampleCountOverflow)?;
        let previous_segment = self.approach_at(self.elapsed).map(|(_, segment)| segment);
        let next_segment = self.approach_at(elapsed).map(|(_, segment)| segment);
        if next_segment != previous_segment {
            self.curve_origin =
                span.source_ratio_at(span.output_frames())
                    .map(|(numerator, denominator)| {
                        (
                            Fraction {
                                numerator,
                                denominator,
                            },
                            elapsed,
                        )
                    });
        } else if self.curve_origin.is_none() {
            self.curve_origin = span.source_ratio_at(0).map(|(numerator, denominator)| {
                (
                    Fraction {
                        numerator,
                        denominator,
                    },
                    self.elapsed,
                )
            });
        }
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
